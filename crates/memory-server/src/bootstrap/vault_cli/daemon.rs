use std::io::Write;
use std::path::Path;

pub(super) async fn detect_matching_daemon(
    app_home: &Path,
    global_db_path: &Path,
) -> Option<crate::cli_client::DaemonInfo> {
    if let Some(info) =
        crate::cli_client::detect_daemon_for_global_db(app_home, global_db_path).await
    {
        return Some(info);
    }

    let info = crate::cli_client::detect_daemon(app_home).await?;
    if daemon_matches_vault_db(&info, global_db_path) {
        Some(info)
    } else {
        eprintln!(
            "[vault] foreign daemon global_db={:?}; using local vault DB",
            info.global_db
        );
        None
    }
}

pub(super) fn daemon_matches_vault_db(
    info: &crate::cli_client::DaemonInfo,
    global_db_path: &Path,
) -> bool {
    crate::cli_client::daemon_global_db_matches(info, global_db_path)
}

#[cfg(unix)]
pub(super) async fn call_daemon_vault_unlock(
    app_home: &Path,
    info: &crate::cli_client::DaemonInfo,
    mut password: String,
) -> Result<String, Box<dyn std::error::Error>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let fifo_path = match (|| -> Result<_, Box<dyn std::error::Error>> {
        let unlock_dir = app_home.join("runtime").join("vault-unlock");
        std::fs::create_dir_all(&unlock_dir)?;
        std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))?;
        let fifo_path = unlock_dir.join(format!(
            "unlock-{}-{}.fifo",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let c_path = CString::new(fifo_path.as_os_str().as_bytes())?;
        let mkfifo_rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        if mkfifo_rc != 0 {
            return Err(format!(
                "create unlock FIFO {}: {}",
                fifo_path.display(),
                std::io::Error::last_os_error()
            )
            .into());
        }
        Ok(fifo_path)
    })() {
        Ok(path) => path,
        Err(err) => {
            crate::vault_crypto::zero_string(&mut password);
            return Err(err);
        }
    };

    let mut args = serde_json::Map::new();
    args.insert(
        "password_fifo_path".to_string(),
        serde_json::json!(fifo_path.display().to_string()),
    );

    let writer_path = fifo_path.clone();
    let writer = tokio::task::spawn_blocking(move || {
        write_password_to_fifo_nonblocking(&writer_path, password)
    });
    let (call_result, writer_result) = tokio::join!(
        crate::cli_client::call_daemon_tool(info, "vault_unlock", args),
        async {
            writer
                .await
                .map_err(|e| format!("unlock FIFO writer task failed: {e}"))?
                .map_err(|e| e.to_string())
        }
    );
    let _ = std::fs::remove_file(&fifo_path);
    writer_result.map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let out = call_result.map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(out)
}

#[cfg(not(unix))]
pub(super) async fn call_daemon_vault_unlock(
    _app_home: &Path,
    _info: &crate::cli_client::DaemonInfo,
    mut password: String,
) -> Result<String, Box<dyn std::error::Error>> {
    crate::vault_crypto::zero_string(&mut password);
    Err("daemon vault unlock password forwarding is disabled on non-Unix platforms".into())
}

#[cfg(unix)]
fn write_password_to_fifo_nonblocking(
    path: &Path,
    mut password: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let result = (|| {
        let c_path = CString::new(path.as_os_str().as_bytes())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd >= 0 {
                let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
                file.write_all(password.as_bytes())?;
                file.flush()?;
                return Ok(());
            }

            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ENXIO) || std::time::Instant::now() >= deadline {
                return Err(format!("open unlock FIFO for writing failed: {err}").into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    })();
    crate::vault_crypto::zero_string(&mut password);
    result
}
