use super::handlers::{handle_vault_init, handle_vault_lock, handle_vault_unlock};
use super::params::{VaultInitParams, VaultUnlockParams};
use super::session::{read_unlock_password_fifo, with_vault_key};
use crate::server_state::MemoryServer;
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

#[tokio::test]
async fn with_vault_key_auto_lock_clears_key_and_provider_cache_atomically() {
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
    assert!(server
        .llm
        .provider_secret_for_tests(&["OPENAI_API_KEY"])
        .is_none());
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
        },
    )
    .await
    .expect_err("mixed password transports should be rejected");

    assert!(
        err.contains("either password or password_fifo_path"),
        "expected mixed-transport rejection, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn read_unlock_password_fifo_reads_secure_runtime_fifo() {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let old_home = std::env::var_os("TACHI_HOME");
    let temp = tempfile::tempdir().expect("temp tachi home");
    std::env::set_var("TACHI_HOME", temp.path());
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

    let password = read_unlock_password_fifo(fifo_path.to_str().unwrap()).expect("read fifo");
    writer.join().expect("writer thread");
    assert_eq!(password, "fifo-password");
    assert!(!fifo_path.exists(), "daemon reader should remove FIFO");

    if let Some(value) = old_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[cfg(unix)]
#[test]
fn read_unlock_password_fifo_rejects_regular_file_without_removing_it() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let old_home = std::env::var_os("TACHI_HOME");
    let temp = tempfile::tempdir().expect("temp tachi home");
    std::env::set_var("TACHI_HOME", temp.path());
    let unlock_dir = temp.path().join("runtime").join("vault-unlock");
    std::fs::create_dir_all(&unlock_dir).expect("unlock dir");
    std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))
        .expect("unlock dir perms");
    let regular_path = unlock_dir.join("not-a-fifo");
    std::fs::write(&regular_path, b"not a fifo").expect("regular file");

    let err = read_unlock_password_fifo(regular_path.to_str().unwrap()).expect_err("regular file");
    assert!(
        err.contains("must be a FIFO"),
        "expected FIFO rejection, got: {err}"
    );
    assert!(
        regular_path.exists(),
        "rejected non-FIFO input should not be removed"
    );

    if let Some(value) = old_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}
