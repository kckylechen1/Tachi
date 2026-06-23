use crate::server_state::MemoryServer;
use crate::vault_crypto as crypto;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const VAULT_UNLOCK_MAX_FAILED_ATTEMPTS: u32 = 5;
const VAULT_UNLOCK_LOCKOUT_SECS: u64 = 300;

fn remaining_lockout_seconds(until: Instant) -> u64 {
    let remaining = until.saturating_duration_since(Instant::now());
    let secs = remaining.as_secs();
    if remaining.subsec_nanos() > 0 {
        secs.saturating_add(1)
    } else {
        secs
    }
}

fn clear_cached_vault_state_locked(v: &mut crate::VaultState) {
    let _ = v.key.take();
    v.unlock_time = None;
}

pub(super) fn clear_cached_vault_state(server: &MemoryServer) {
    {
        let mut v = server.vault_write();
        clear_cached_vault_state_locked(&mut v);
    }
    server.llm.clear_provider_secrets();
}

pub(super) fn maybe_auto_lock_vault(server: &MemoryServer) -> bool {
    let locked = {
        let mut v = server.vault_write();
        let expired = v.unlock_time.is_some_and(|unlock_time| {
            unlock_time.elapsed() > Duration::from_secs(v.auto_lock_after_secs)
        });
        if expired {
            clear_cached_vault_state_locked(&mut v);
        }
        expired
    };
    if locked {
        crate::provider_config::re_materialize_provider_secrets_after_auto_lock(server);
    }
    locked
}

/// Check if vault is unlocked and run work with a borrowed cached key.
pub(super) fn with_vault_key<T>(
    server: &MemoryServer,
    f: impl FnOnce(&[u8; 32]) -> Result<T, String>,
) -> Result<T, String> {
    let mut key_bytes = {
        let mut v = server.vault_write();
        let Some(unlock_time) = v.unlock_time else {
            return Err("Vault is locked. Call vault_unlock first.".to_string());
        };

        if unlock_time.elapsed() > Duration::from_secs(v.auto_lock_after_secs) {
            clear_cached_vault_state_locked(&mut v);
            drop(v);
            crate::provider_config::re_materialize_provider_secrets_after_auto_lock(server);
            return Err("Vault auto-locked. Call vault_unlock first.".into());
        }

        let key = v
            .key
            .as_ref()
            .ok_or_else(|| "Vault is locked. Call vault_unlock first.".to_string())?;
        *key.bytes()
    };

    let result = f(&key_bytes);
    crypto::zero_key(&mut key_bytes);
    result
}

pub(super) fn ensure_vault_unlocked(server: &MemoryServer) -> Result<(), String> {
    with_vault_key(server, |_| Ok(()))
}

/// Check if vault is initialized.
pub(super) fn is_vault_initialized(server: &MemoryServer) -> Result<bool, String> {
    server
        .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
        .map(|opt| opt.is_some())
}

pub(super) fn ensure_vault_unlock_allowed(server: &MemoryServer) -> Result<(), String> {
    let v = server.vault_read();
    if let Some(until) = v.failed_attempts.1 {
        if Instant::now() < until {
            return Err(format!(
                "Vault unlock temporarily locked. Try again in {} seconds.",
                remaining_lockout_seconds(until)
            ));
        }
    }
    Ok(())
}

pub(super) fn record_vault_unlock_failure(server: &MemoryServer) -> Result<String, String> {
    let mut v = server.vault_write();
    v.failed_attempts.0 = v.failed_attempts.0.saturating_add(1);
    if v.failed_attempts.0 >= VAULT_UNLOCK_MAX_FAILED_ATTEMPTS {
        let until = Instant::now() + Duration::from_secs(VAULT_UNLOCK_LOCKOUT_SECS);
        v.failed_attempts.1 = Some(until);
        return Err(format!(
            "Too many failed vault unlock attempts. Try again in {} seconds.",
            remaining_lockout_seconds(until)
        ));
    }
    Err("Wrong password".to_string())
}

#[cfg(unix)]
pub(super) fn read_unlock_password_fifo(path: &str) -> Result<String, String> {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let path = PathBuf::from(path);
    let unlock_dir = crate::path_utils::tachi_home()
        .join("runtime")
        .join("vault-unlock");
    let expected_parent = unlock_dir
        .canonicalize()
        .map_err(|e| format!("unlock FIFO directory is not available: {e}"))?;
    let actual_parent = path
        .parent()
        .ok_or_else(|| "unlock FIFO path has no parent".to_string())?
        .canonicalize()
        .map_err(|e| format!("unlock FIFO parent is not available: {e}"))?;
    if actual_parent != expected_parent {
        return Err("unlock FIFO path is outside the Tachi runtime unlock directory".to_string());
    }
    let parent_meta = std::fs::metadata(&actual_parent)
        .map_err(|e| format!("unlock FIFO parent metadata failed: {e}"))?;
    if parent_meta.permissions().mode() & 0o077 != 0 {
        return Err(
            "unlock FIFO directory permissions must not allow group/other access".to_string(),
        );
    }

    let mut should_remove_fifo = false;
    let result = (|| {
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|e| format!("unlock FIFO path contains interior NUL: {e}"))?;
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(format!(
                "open unlock FIFO failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
            let err = std::io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(format!("unlock FIFO fstat failed: {err}"));
        }
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFIFO {
            unsafe {
                libc::close(fd);
            }
            return Err("unlock password path must be a FIFO".to_string());
        }
        if stat.st_mode & 0o077 != 0 {
            unsafe {
                libc::close(fd);
            }
            return Err("unlock FIFO permissions must not allow group/other access".to_string());
        }
        should_remove_fifo = true;

        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 512];
        loop {
            match file.read(&mut buffer) {
                Ok(0) if !bytes.is_empty() => break,
                Ok(0) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(0) => return Err("timed out waiting for unlock FIFO password".to_string()),
                Ok(n) => {
                    bytes.extend_from_slice(&buffer[..n]);
                    if bytes.len() > 4096 {
                        return Err("unlock FIFO password exceeded maximum length".to_string());
                    }
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::Interrupted =>
                {
                    if std::time::Instant::now() >= deadline {
                        return Err("timed out waiting for unlock FIFO password".to_string());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(err) => return Err(format!("read unlock FIFO failed: {err}")),
            }
        }
        String::from_utf8(bytes).map_err(|e| format!("unlock FIFO password is not UTF-8: {e}"))
    })();
    if should_remove_fifo {
        let _ = std::fs::remove_file(&path);
    }
    let password = result?;
    if password.is_empty() {
        return Err("unlock FIFO did not contain a password".to_string());
    }
    Ok(password)
}

#[cfg(not(unix))]
pub(super) fn read_unlock_password_fifo(_path: &str) -> Result<String, String> {
    Err("unlock password FIFO transport is only supported on Unix".to_string())
}
