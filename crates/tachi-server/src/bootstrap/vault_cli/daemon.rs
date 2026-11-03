use std::io::Write;
use std::path::Path;

const VAULT_ADMIN_PATH: &str = "/internal/vault";

/// Owner-only daemon operations. Empty struct variants retain strict field checks.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::bootstrap) enum VaultAdminRequest {
    Status {},
    Lock {},
    UnlockFifo {
        password_fifo_path: String,
    },
    List {},
    Lease {
        name: String,
        env_name: Option<String>,
    },
    RecordKeyResult {
        logical_name: String,
        key_id: String,
        status_code: Option<u16>,
        outcome: Option<String>,
        retry_after_secs: Option<u64>,
        reason: Option<String>,
    },
    SetupRotation {
        prefix: String,
        total_keys: i64,
        strategy: String,
    },
}

pub(in crate::bootstrap) fn vault_admin_router(server: crate::MemoryServer) -> axum::Router {
    axum::Router::new()
        .route(VAULT_ADMIN_PATH, axum::routing::post(handle_vault_admin))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .with_state(server)
}

async fn handle_vault_admin(
    axum::extract::State(server): axum::extract::State<crate::MemoryServer>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<axum::Json<serde_json::Value>, (axum::http::StatusCode, axum::Json<serde_json::Value>)>
{
    use crate::vault_ops::*;
    use axum::http::StatusCode;
    let error =
        |status, message: String| (status, axum::Json(serde_json::json!({"error": message})));
    let token = headers
        .get(crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN)
        .and_then(|value| value.to_str().ok());
    if !server.admits_internal_proxy_token(token) {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "Vault owner authentication required".into(),
        ));
    }
    let request: VaultAdminRequest = serde_json::from_slice(&body).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "Invalid Vault management request".into(),
        )
    })?;
    server.touch_activity();
    let result = match request {
        VaultAdminRequest::Status {} => handle_vault_status(&server).await,
        VaultAdminRequest::Lock {} => handle_vault_lock(&server).await,
        VaultAdminRequest::UnlockFifo { password_fifo_path } => {
            handle_vault_unlock(
                &server,
                VaultUnlockParams {
                    password: String::new(),
                    password_fifo_path: Some(password_fifo_path),
                    use_keychain: false,
                },
            )
            .await
        }
        VaultAdminRequest::List {} => {
            handle_vault_list(&server, VaultListParams { secret_type: None }).await
        }
        VaultAdminRequest::Lease { name, env_name } => {
            handle_vault_lease_api_key(
                &server,
                VaultLeaseApiKeyParams {
                    name,
                    env_name,
                    agent_id: None,
                },
            )
            .await
        }
        VaultAdminRequest::RecordKeyResult {
            logical_name,
            key_id,
            status_code,
            outcome,
            retry_after_secs,
            reason,
        } => {
            handle_vault_record_key_result(
                &server,
                VaultRecordKeyResultParams {
                    logical_name,
                    key_id,
                    status_code,
                    outcome,
                    retry_after_secs,
                    reason,
                },
            )
            .await
        }
        VaultAdminRequest::SetupRotation {
            prefix,
            total_keys,
            strategy,
        } => {
            handle_vault_setup_rotation(
                &server,
                VaultSetupRotationParams {
                    prefix,
                    total_keys,
                    strategy,
                    agent_id: None,
                },
            )
            .await
        }
    }
    .map_err(|message| error(StatusCode::BAD_REQUEST, message))?;
    serde_json::from_str(&result).map(axum::Json).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Invalid Vault management response".into(),
        )
    })
}

pub(in crate::bootstrap) async fn call_daemon_vault(
    info: &crate::cli_client::DaemonInfo,
    request: &VaultAdminRequest,
) -> Result<String, Box<dyn std::error::Error>> {
    let token = info
        .internal_proxy_token
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or("Daemon has no owner capability; restart it with the current Tachi version")?;
    let mut url = reqwest::Url::parse(&info.url)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/mcp"
    {
        return Err("Vault management requires a verified loopback daemon URL".into());
    }
    url.set_path(VAULT_ADMIN_PATH);
    let mut capability = reqwest::header::HeaderValue::from_str(token)
        .map_err(|_| "Invalid daemon owner capability")?;
    capability.set_sensitive(true);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let response = client
        .post(url)
        .header(
            crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN,
            capability,
        )
        .json(request)
        .send()
        .await?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("Daemon does not support Vault owner management; restart it with the current Tachi version".into());
    }
    let body = response.text().await?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| value["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("Vault management failed with HTTP {status}"));
        return Err(message.into());
    }
    Ok(body)
}

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
        // SAFETY: `c_path` is a valid NUL-terminated CString outliving the call.
        // `mkfifo` only reads the path and a mode bitmask; it returns an integer
        // status and aliases no Rust memory. The FIFO is created under a 0o700
        // owner-only directory (set above) with mode 0o600.
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

    let request = VaultAdminRequest::UnlockFifo {
        password_fifo_path: fifo_path.display().to_string(),
    };

    let writer_path = fifo_path.clone();
    let writer = tokio::task::spawn_blocking(move || {
        write_password_to_fifo_nonblocking(&writer_path, password)
    });
    let (call_result, writer_result) = tokio::join!(call_daemon_vault(info, &request), async {
        writer
            .await
            .map_err(|e| format!("unlock FIFO writer task failed: {e}"))?
            .map_err(|e| e.to_string())
    });
    let _ = std::fs::remove_file(&fifo_path);
    // Check the daemon call's own error first: if the daemon rejected the
    // tool call (e.g. "tool not found"), no reader ever opens the FIFO and
    // the writer times out with ENXIO. That ENXIO is expected collateral,
    // not the real error — surfacing it instead of call_result's error
    // masks the actual failure (#979).
    match call_result {
        Ok(out) => {
            // Daemon call succeeded but the password write to the FIFO
            // failed — that's a genuine, informative failure.
            writer_result.map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            Ok(out)
        }
        Err(call_err) => Err(call_err.into()),
    }
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
            // SAFETY: `c_path` is a valid NUL-terminated CString outliving the
            // call. `open` only reads the path and returns an integer fd; it
            // hands no pointers back into Rust memory.
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd >= 0 {
                // SAFETY: `fd` is a freshly-returned valid descriptor (≥ 0)
                // not yet owned by any `File`. `from_raw_fd` takes single
                // ownership; `file` closes the descriptor on drop, so there
                // is no double-close hazard.
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
