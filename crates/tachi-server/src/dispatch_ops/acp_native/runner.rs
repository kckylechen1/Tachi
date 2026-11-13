use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::super::dispatch::{DispatchResult, DispatchRunOutcome};
use super::super::dispatch_v2::append_trajectory_event;
use super::session::write_native_acp_session_record;
use super::{
    NativeAcpConnection, NativeAcpDeferredArtifacts, NativeAcpEventTarget, NativeAcpPromptOutcome,
    NativeAcpRunSpec, ACP_STREAM_FILE,
};

/// Credential files a native adapter may hold in its private runtime: the
/// snapshot itself plus opencode v2's import of it into a local credential store.
const NATIVE_AUTH_FILES: [&std::ffi::CStr; 4] = [
    c"auth.json",
    c"opencode.db",
    c"opencode.db-wal",
    c"opencode.db-shm",
];

#[derive(Default)]
struct NativeAuthCleanup {
    #[cfg(unix)]
    parents: Vec<std::fs::File>,
}
impl NativeAuthCleanup {
    fn cleanup(&mut self) -> Result<(), String> {
        #[cfg(unix)]
        for parent in &self.parents {
            use std::os::fd::AsRawFd;
            // Never traverse a worker-replaced path. A renamed parent is still
            // this run's exact opened directory, including refreshed auth.
            for name in NATIVE_AUTH_FILES {
                if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ENOENT) {
                        return Err(format!("native provider auth cleanup failed: {error}"));
                    }
                }
            }
        }
        #[cfg(unix)]
        self.parents.clear();
        Ok(())
    }
}
impl Drop for NativeAuthCleanup {
    fn drop(&mut self) {
        // Cancellation/panic fallback only; every ordinary return explicitly
        // propagates this obligation to the canonical terminal finalizer.
        let _ = self.cleanup();
    }
}

fn native_command(
    spec: &NativeAcpRunSpec,
    run_dir: &Path,
) -> Result<(Command, NativeAuthCleanup), String> {
    let mut cleanup = NativeAuthCleanup::default();
    let mut command = if spec.managed {
        let runtime = run_dir
            .canonicalize()
            .map_err(|e| format!("resolve private native run directory: {e}"))?
            .join("runtime");
        std::fs::create_dir_all(&runtime)
            .map_err(|e| format!("create private native runtime: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let cwd = spec
            .sandbox_cwd
            .as_deref()
            .ok_or("native profile has no managed sandbox workspace")?;
        let invocation = tachi_dispatch::native_sandbox_command(
            std::ffi::OsStr::new(&spec.command),
            &spec
                .args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>(),
            cwd,
            &runtime,
            spec.metadata["profile_contract"]["writable"] == true,
        )?;
        let mut command = Command::new(invocation.program);
        command.args(invocation.args).env_clear();
        for key in [
            "PATH",
            "LANG",
            "LC_ALL",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "NO_PROXY",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        // Isolation variables are server-owned and cannot be widened by a profile's credentials.
        for (key, value) in invocation.env {
            command.env(key, value);
        }
        snapshot_provider_auth(spec, &runtime, &mut command, &mut cleanup)?;
        command
    } else {
        let mut command = Command::new(&spec.command);
        command.args(&spec.args);
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        command
    };
    if spec.cwd_authority.is_none() {
        command.current_dir(&spec.cwd);
    }
    for key in &spec.env_remove {
        command.env_remove(key);
    }
    Ok((command, cleanup))
}

/// The fixed native credential policy snapshots one provider credential, never a
/// host config, history, MCP definition or writable provider home. Refreshed
/// tokens stay private and are removed with the run.
fn snapshot_provider_auth(
    spec: &NativeAcpRunSpec,
    runtime: &Path,
    command: &mut Command,
    cleanup: &mut NativeAuthCleanup,
) -> Result<(), String> {
    let host_home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let contract = &spec.metadata["profile_contract"];
    let backend = contract["backend"].as_str().unwrap_or("");
    let unavailable = || {
        format!("native {backend} authentication snapshot is unavailable; configure the provider account before starting; no adapter process was spawned")
    };
    let (source, destination) = match backend {
        "codex" => {
            let source = spec
                .env
                .get("CODEX_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from))
                .or_else(|| host_home.as_ref().map(|home| home.join(".codex")));
            let target = runtime.join("home/.codex");
            command.env("CODEX_HOME", &target);
            (
                source.map(|dir| SnapshotSource::File(dir.join("auth.json"))),
                target.join("auth.json"),
            )
        }
        "opencode" => {
            let store = spec
                .env
                .get("XDG_DATA_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from))
                .or_else(|| host_home.as_ref().map(|home| home.join(".local/share")))
                .map(|dir| dir.join("opencode"));
            // opencode v2 keeps the live (refreshed) credentials in its db and
            // leaves a migrated auth.json behind, so a credential db wins. Older
            // releases keep sessions in the same db file and auth in auth.json.
            let source = store.map(|store| {
                let db = store.join("opencode.db");
                let legacy = store.join("auth.json");
                if db.is_file() {
                    SnapshotSource::OpencodeDb { db, legacy }
                } else {
                    SnapshotSource::File(legacy)
                }
            });
            (source, runtime.join("data/opencode/auth.json"))
        }
        _ => return Err("native profile has no declared credential policy".into()),
    };
    std::fs::create_dir_all(destination.parent().expect("provider auth parent"))
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let parent = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(destination.parent().expect("provider auth parent"))
            .map_err(|e| e.to_string())?;
        cleanup.parents.push(parent);
    }
    #[cfg(not(unix))]
    let _ = cleanup;
    let read_file = |source: &Path| -> Result<Vec<u8>, String> {
        if !source.is_file() {
            return Err(unavailable());
        }
        let metadata = std::fs::metadata(source).map_err(|e| e.to_string())?;
        if metadata.len() > NATIVE_AUTH_SNAPSHOT_MAX_BYTES as u64 {
            return Err("native provider auth snapshot exceeds 1 MiB".into());
        }
        std::fs::read(source).map_err(|_| "native provider auth snapshot is unreadable".into())
    };
    let body = match source.ok_or_else(unavailable)? {
        SnapshotSource::File(source) => read_file(&source)?,
        SnapshotSource::OpencodeDb { db, legacy } => {
            let model = contract["model"].as_str().unwrap_or("");
            let lifetime = contract["lifetime_timeout_secs"].as_u64().unwrap_or(0);
            match opencode_db_auth_snapshot(&db, model, lifetime, Utc::now().timestamp())? {
                Some(body) => body,
                None => read_file(&legacy)?,
            }
        }
    };
    if body.len() > NATIVE_AUTH_SNAPSHOT_MAX_BYTES {
        return Err("native provider auth snapshot exceeds 1 MiB".into());
    }
    if !serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .is_some_and(|value| value.as_object().is_some_and(|object| !object.is_empty()))
    {
        return Err("native provider auth snapshot is not a nonempty JSON object; no adapter process was spawned".into());
    }
    crate::utils::write_owner_only_file_atomic(&destination, &body)?;
    Ok(())
}

enum SnapshotSource {
    File(std::path::PathBuf),
    OpencodeDb {
        db: std::path::PathBuf,
        legacy: std::path::PathBuf,
    },
}

const NATIVE_AUTH_SNAPSHOT_MAX_BYTES: usize = 1024 * 1024;

/// Refuse OAuth snapshots whose access token could expire mid-run: a refresh
/// inside the sandbox may rotate the refresh token and sign out the host copy.
const OAUTH_EXPIRY_MARGIN_SECS: i64 = 600;

/// Transcode the pinned provider's single active opencode v2 credential into the
/// legacy auth.json shape, which opencode imports into the private runtime store.
/// `None` means the db predates the credential store (auth lives in auth.json).
/// Error text never includes credential material.
fn opencode_db_auth_snapshot(
    db: &Path,
    model: &str,
    lifetime_secs: u64,
    now_secs: i64,
) -> Result<Option<Vec<u8>>, String> {
    use rusqlite::{Connection, OpenFlags};
    use serde_json::{Map, Value};

    let provider = model
        .split_once('/')
        .map(|(provider, _)| provider)
        .filter(|provider| !provider.is_empty())
        .ok_or(
            "native opencode profile model has no provider prefix; no adapter process was spawned",
        )?;
    let store_error = |_| {
        "native opencode credential store is unreadable or has an unsupported layout; no adapter process was spawned".to_string()
    };
    let connection = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(store_error)?;
    let has_credential_store: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'credential')",
            [],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    if !has_credential_store {
        return Ok(None);
    }
    // Oversized values come back NULL so they are never materialized.
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN length(CAST(value AS BLOB)) <= ?2 THEN value END FROM credential \
             WHERE integration_id = ?1 AND (active = 1 OR active IS NULL) LIMIT 2",
        )
        .map_err(store_error)?;
    let rows = statement
        .query_map(
            rusqlite::params![provider, NATIVE_AUTH_SNAPSHOT_MAX_BYTES as i64],
            |row| row.get::<_, Option<String>>(0),
        )
        .map_err(store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_error)?;
    let value = match rows.as_slice() {
        [] => return Err(format!("native opencode authentication snapshot has no active {provider} credential; run `opencode auth login` on the host before starting; no adapter process was spawned")),
        [Some(value)] => value,
        [None] => return Err("native provider auth snapshot exceeds 1 MiB".into()),
        _ => return Err(format!("native opencode credential store has several active {provider} credentials; keep exactly one active account before starting; no adapter process was spawned")),
    };
    let stored: Map<String, Value> = serde_json::from_str(value)
        .map_err(|_| format!("native opencode {provider} credential is not a JSON object; no adapter process was spawned"))?;
    let text = |field: &str| {
        stored
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| Value::String(value.to_string()))
    };
    let malformed = || {
        format!("native opencode {provider} credential is incomplete; re-authenticate on the host before starting; no adapter process was spawned")
    };
    let entry = match stored.get("type").and_then(Value::as_str) {
        Some("key") => serde_json::json!({"type": "api", "key": text("key").ok_or_else(malformed)?}),
        Some("oauth") => {
            let expires_ms = stored
                .get("expires")
                .and_then(Value::as_i64)
                .ok_or_else(malformed)?;
            let required_secs = i64::try_from(lifetime_secs)
                .unwrap_or(i64::MAX)
                .saturating_add(OAUTH_EXPIRY_MARGIN_SECS);
            if expires_ms / 1000 - now_secs < required_secs {
                return Err(format!("native opencode {provider} OAuth access token expires before the run lifetime ends; refresh it by using opencode on the host before starting; no adapter process was spawned"));
            }
            let mut entry = serde_json::json!({
                "type": "oauth",
                "refresh": text("refresh").ok_or_else(malformed)?,
                "access": text("access").ok_or_else(malformed)?,
                "expires": expires_ms,
            });
            if let Some(account) = stored
                .get("metadata")
                .and_then(|metadata| metadata.get("accountID"))
                .and_then(Value::as_str)
                .filter(|account| !account.is_empty())
            {
                entry["accountId"] = Value::String(account.to_string());
            }
            entry
        }
        _ => return Err(format!("native opencode {provider} credential type is unsupported; no adapter process was spawned")),
    };
    serde_json::to_vec(&serde_json::json!({ provider: entry }))
        .map(Some)
        .map_err(|e| format!("encode native opencode auth snapshot: {e}"))
}

#[cfg(test)]
pub(in crate::dispatch_ops) async fn run_native_acp_dispatch(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    run_native_acp_dispatch_with_liveness(
        spec,
        run_dir,
        trajectory_path,
        dispatch_id,
        agent,
        timeout,
        false,
    )
    .await
    .result
}

pub(in crate::dispatch_ops) async fn run_native_acp_dispatch_with_liveness(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
    defer_artifacts: bool,
) -> DispatchRunOutcome {
    // The inner runner owns the timeout after it has spawned the adapter, so a
    // timeout cannot drop the future before the parent retains its process
    // group identity for postflight liveness probing.
    run_native_acp_dispatch_inner(
        spec,
        run_dir,
        trajectory_path,
        dispatch_id,
        agent,
        timeout,
        defer_artifacts,
        None,
        None,
        &mut None,
    )
    .await
}

pub(in crate::dispatch_ops) async fn run_managed_native_acp(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
    defer_artifacts: bool,
    mut control: crate::managed_run_control::native_session::NativeSessionControl,
) -> (
    DispatchRunOutcome,
    Option<crate::managed_run_control::ManagedCancelCommand>,
    Option<&'static str>,
) {
    let initial = super::supervised::initial_projection(&spec);
    let initialized =
        crate::utils::write_owner_only_file_atomic(&run_dir.join("events.jsonl"), b"").and_then(
            |()| {
                crate::managed_run_control::native_session::update_phase(
                    &control.server,
                    dispatch_id,
                    None,
                    initial,
                )
                .map(|_| ())
            },
        );
    if let Err(error) = initialized {
        return (
            DispatchRunOutcome::failure(
                error,
                crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned,
            ),
            None,
            Some("spawn_suppressed"),
        );
    }
    let failure_server = control.server.clone();
    let mut receiver = control
        .cancellation
        .take()
        .expect("registered native cancellation receiver");
    let mut cancelled = None;
    let result = run_native_acp_dispatch_inner(
        spec,
        run_dir,
        trajectory_path,
        dispatch_id,
        agent,
        timeout,
        defer_artifacts,
        Some(control),
        Some(&mut receiver),
        &mut cancelled,
    )
    .await;
    #[cfg(unix)]
    if let Err(error) = &result.result {
        if let Some(status) = failure_server
            .managed_run_controls
            .accepted_status_anchor(dispatch_id)
            .and_then(|anchor| anchor.read_json().ok().flatten())
        {
            let mut projection = status["native_session"].clone();
            if projection["phase"] != "failed" && projection["phase"] != "cancelled" {
                projection["phase"] = json!(if cancelled.is_some() {
                    "cancelled"
                } else {
                    "failed"
                });
                projection["failure_class"] = json!(super::supervised::classify_failure(error));
                projection["failure"] = json!(error);
                let _ = crate::managed_run_control::native_session::update_phase(
                    &failure_server,
                    dispatch_id,
                    None,
                    projection,
                );
            }
        }
    }
    let proof = match &result.liveness {
        crate::exec_env_postflight::RunnerLivenessEvidence::ConfirmedReaped { .. } => {
            // The existing cancellation vocabulary names the proven owned
            // group fact. Detailed confinement proof remains in liveness.
            Some("unix_process_group_absent")
        }
        crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned => {
            Some("spawn_suppressed")
        }
        _ => None,
    };
    (result, cancelled, proof)
}

async fn run_native_acp_dispatch_inner(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
    defer_artifacts: bool,
    control: Option<crate::managed_run_control::native_session::NativeSessionControl>,
    cancels: Option<
        &mut tokio::sync::mpsc::Receiver<crate::managed_run_control::ManagedCancelCommand>,
    >,
    cancelled: &mut Option<crate::managed_run_control::ManagedCancelCommand>,
) -> DispatchRunOutcome {
    let (cmd, mut auth_cleanup) = match native_command(&spec, run_dir) {
        Ok(command) => command,
        Err(error) => {
            return DispatchRunOutcome::failure(
                error,
                crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned,
            )
        }
    };
    let server = control.as_ref().map(|control| control.server.clone());
    let mut outcome = run_native_acp_command_inner(
        spec,
        cmd,
        run_dir,
        trajectory_path,
        dispatch_id,
        agent,
        timeout,
        defer_artifacts,
        control,
        cancels,
        cancelled,
    )
    .await;
    if let Err(error) = auth_cleanup.cleanup() {
        outcome.credential_cleanup_error = Some(error.clone());
        if cancelled.is_none() {
            outcome.result = Err(error.clone());
        }
        #[cfg(unix)]
        if let Some(server) = server.as_ref() {
            if let Some(mut status) = server
                .managed_run_controls
                .accepted_status_anchor(dispatch_id)
                .and_then(|anchor| anchor.read_json().ok().flatten())
            {
                let mut projection = status["native_session"].take();
                if projection.is_object() {
                    projection["phase"] = json!("failed");
                    projection["failure_class"] = json!("credential_cleanup");
                    projection["failure"] = json!(error);
                    let _ = crate::managed_run_control::native_session::update_phase(
                        server,
                        dispatch_id,
                        None,
                        projection,
                    );
                }
            }
        }
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn run_native_acp_command_inner(
    spec: NativeAcpRunSpec,
    mut cmd: Command,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
    defer_artifacts: bool,
    mut control: Option<crate::managed_run_control::native_session::NativeSessionControl>,
    mut cancels: Option<
        &mut tokio::sync::mpsc::Receiver<crate::managed_run_control::ManagedCancelCommand>,
    >,
    cancelled: &mut Option<crate::managed_run_control::ManagedCancelCommand>,
) -> DispatchRunOutcome {
    let escape_contained = spec.managed
        || (defer_artifacts
            && crate::dispatch_ops::subprocess::configure_required_postflight_containment(
                &mut cmd,
            ));
    if let Some(authority) = spec.cwd_authority.as_ref() {
        if let Err(error) = authority.anchor_command_cwd(cmd.as_std_mut()) {
            return DispatchRunOutcome::failure(
                format!("pin native ACP child cwd after containment wrapping: {error}"),
                crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned,
            );
        }
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    cmd.kill_on_drop(false);
    #[cfg(not(unix))]
    cmd.kill_on_drop(true);
    crate::dispatch_ops::subprocess::configure_process_group(&mut cmd);
    #[cfg(unix)]
    if spec.managed {
        // Must run after the descriptor-bound fchdir hook. CLOEXEC preserves
        // Rust's spawn-error pipe until exec and removes ambient writable FDs.
        unsafe {
            cmd.pre_exec(tachi_dispatch::mark_native_inherited_fds_cloexec);
        }
    }

    if let Some(receiver) = cancels.as_mut() {
        match receiver.try_recv() {
            Ok(command) => {
                *cancelled = Some(command);
                return DispatchRunOutcome::failure(
                    "managed_cancelled",
                    crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned,
                );
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return DispatchRunOutcome::failure(
                    "native controller lost before spawn",
                    crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned,
                );
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        }
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return DispatchRunOutcome::failure(
                format!("Failed to spawn native ACP adapter: {error}"),
                crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned,
            )
        }
    };
    crate::staffing_ops::presence::worker_event("starting", "acp_process_spawned");
    let child_pid = child.id();
    #[cfg(unix)]
    let mut process_group =
        crate::dispatch_ops::subprocess::ManagedProcessGroupGuard::arm(child_pid);
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Native ACP adapter stdin was not piped".to_string());
    let stdin = match stdin {
        Ok(stdin) => stdin,
        Err(error) => {
            #[cfg(unix)]
            let (_, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
                &mut child,
                &mut process_group,
                escape_contained,
            )
            .await;
            #[cfg(not(unix))]
            let liveness =
                crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child).await;
            return DispatchRunOutcome::failure(error, liveness);
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Native ACP adapter stdout was not piped".to_string());
    let stdout = match stdout {
        Ok(stdout) => stdout,
        Err(error) => {
            #[cfg(unix)]
            let (_, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
                &mut child,
                &mut process_group,
                escape_contained,
            )
            .await;
            #[cfg(not(unix))]
            let liveness =
                crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child).await;
            return DispatchRunOutcome::failure(error, liveness);
        }
    };
    let mut stderr = match child
        .stderr
        .take()
        .ok_or_else(|| "Native ACP adapter stderr was not piped".to_string())
    {
        Ok(stderr) => stderr,
        Err(error) => {
            #[cfg(unix)]
            let (_, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
                &mut child,
                &mut process_group,
                escape_contained,
            )
            .await;
            #[cfg(not(unix))]
            let liveness =
                crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child).await;
            return DispatchRunOutcome::failure(error, liveness);
        }
    };
    let stderr_task = tokio::spawn(async move {
        let mut captured = String::new();
        let _ = stderr.read_to_string(&mut captured).await;
        captured
    });

    let mut connection = NativeAcpConnection::new(
        stdin,
        stdout,
        &spec,
        dispatch_id,
        agent,
        run_dir,
        trajectory_path,
        defer_artifacts,
    );
    let outcome = {
        let work = async {
            if let Some(control) = control.as_mut() {
                super::supervised::run_supervised(&mut connection, &spec, control, run_dir).await
            } else {
                connection.run_prompt_turn(&spec).await
            }
        };
        tokio::select! {
            biased;
            command = async { match cancels.as_mut() {
                Some(receiver) => receiver.recv().await,
                None => std::future::pending().await,
            }} => {
                *cancelled = command;
                Err(if cancelled.is_some() {"managed_cancelled"} else {"native controller lost; no automatic retry"}.to_string())
            },
            result = tokio::time::timeout(timeout, work) => result.unwrap_or_else(|_| Err(format!("Native ACP dispatch timed out after {}s", timeout.as_secs()))),
        }
    };
    if let (Err(error), Some(control)) = (&outcome, control.as_ref()) {
        let phase = if cancelled.is_some() {
            "cancelled"
        } else {
            "failed"
        };
        let mut projection =
            super::supervised::projection(&connection, &spec, connection.turn_number, phase, None);
        projection["failure_class"] = json!(if cancelled.is_some() {
            "cancelled"
        } else {
            super::supervised::classify_failure(error)
        });
        projection["failure"] = json!(error);
        let _ = crate::managed_run_control::native_session::update_phase(
            &control.server,
            dispatch_id,
            None,
            projection,
        );
    }
    if cancelled.is_some() {
        let _ = tokio::time::timeout(Duration::from_secs(2), connection.cancel_session()).await;
    }
    let close_result = connection.close_stdin().await;

    #[cfg(unix)]
    let root_exit = crate::dispatch_ops::subprocess::wait_for_owned_root_exit(
        child_pid,
        Duration::from_millis(1500),
    )
    .await;
    #[cfg(unix)]
    let (status, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
        &mut child,
        &mut process_group,
        escape_contained,
    )
    .await;
    #[cfg(unix)]
    let (process_exit_code, process_exit_error) = match (root_exit, status) {
        (Ok(true), Ok(status)) => (status.code(), None),
        (Err(err), _) | (_, Err(err)) => {
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_process_wait_failed",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "error": err.to_string(),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            (None, Some(format!("Native ACP adapter wait failed: {err}")))
        }
        (Ok(false), Ok(_)) => {
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_process_killed_after_turn",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "reason": "adapter did not exit after stdin close",
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            (
                None,
                Some("Native ACP adapter did not exit after stdin close".to_string()),
            )
        }
    };
    #[cfg(not(unix))]
    let (process_exit_code, process_exit_error, liveness) = {
        let graceful_exit = tokio::time::timeout(Duration::from_millis(1500), child.wait()).await;
        match graceful_exit {
            Ok(Ok(status)) => (
                status.code(),
                None,
                crate::exec_env_postflight::RunnerLivenessEvidence::indeterminate(
                    "native ACP root child was reaped; descendant containment is unavailable on this platform",
                ),
            ),
            Ok(Err(err)) => {
                let liveness =
                    crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child)
                        .await;
                (
                    None,
                    Some(format!("Native ACP adapter wait failed: {err}")),
                    liveness,
                )
            }
            Err(_) => {
                let liveness =
                    crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child)
                        .await;
                (
                    None,
                    Some("Native ACP adapter did not exit after stdin close".to_string()),
                    liveness,
                )
            }
        }
    };
    let stderr_output = stderr_task.await.unwrap_or_default();

    if outcome.is_ok() {
        let error = process_exit_error.or_else(|| {
            (process_exit_code != Some(0))
                .then(|| format!("Native ACP adapter exited with status {process_exit_code:?}"))
        });
        if let Some(error) = error {
            return DispatchRunOutcome::failure(error, liveness);
        }
    }

    if let Err(err) = close_result {
        append_trajectory_event(
            trajectory_path,
            json!({
                "event": "acp_native_stdin_close_failed",
                "dispatch_id": dispatch_id,
                "agent": agent,
                "error": err,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
    }
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            if err == "managed_cancelled" {
                return DispatchRunOutcome::failure(err, liveness);
            }
            let stderr_tail = crate::dispatch_ops::subprocess::tail_chars(&stderr_output, 4000);
            let suffix = if stderr_tail.is_empty() {
                String::new()
            } else {
                format!("; adapter stderr: {stderr_tail}")
            };
            return DispatchRunOutcome::failure(format!("{err}{suffix}"), liveness);
        }
    };

    let result = DispatchResult {
        output: outcome.output.clone(),
        exit_code: process_exit_code,
        observed_model: outcome.observed_model.clone(),
    };
    let deferred = NativeAcpDeferredArtifacts {
        spec,
        outcome,
        process_exit_code,
    };
    if defer_artifacts {
        DispatchRunOutcome {
            result: Ok(result),
            liveness,
            deferred_native_acp: Some(deferred),
            credential_cleanup_error: None,
        }
    } else if let Err(error) =
        publish_native_acp_artifacts(deferred, run_dir, trajectory_path, dispatch_id, agent, true)
    {
        DispatchRunOutcome::failure(error, liveness)
    } else {
        DispatchRunOutcome::success(result, liveness)
    }
}

pub(in crate::dispatch_ops) fn publish_native_acp_artifacts(
    deferred: NativeAcpDeferredArtifacts,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    publish_worker_mappings: bool,
) -> Result<(), String> {
    let NativeAcpDeferredArtifacts {
        spec,
        outcome,
        process_exit_code,
    } = deferred;
    let stream_path = run_dir.join(ACP_STREAM_FILE);

    // Required-postflight publication makes the raw stream its sole atomic
    // worker-authored artifact. Session continuity metadata is deliberately
    // stripped of prompt/output material and written first, so a later raw
    // stream failure cannot leave carrier output visible.
    if !publish_worker_mappings {
        if let Some(record_path) = spec.session_record_path.as_ref() {
            write_native_acp_session_record(
                &spec,
                record_path,
                spec.session_distill_path.as_ref(),
                &outcome,
                &stream_path,
                dispatch_id,
                false,
            )?;
        }
    }
    persist_raw_stream(&outcome, run_dir)?;
    if publish_worker_mappings {
        if let Some(record_path) = spec.session_record_path.as_ref() {
            write_native_acp_session_record(
                &spec,
                record_path,
                spec.session_distill_path.as_ref(),
                &outcome,
                &stream_path,
                dispatch_id,
                true,
            )?;
        }
    }
    for event in &outcome.staged_events {
        if publish_worker_mappings
            || matches!(event.target, NativeAcpEventTarget::PostflightReceipt)
        {
            let target = match event.target {
                NativeAcpEventTarget::Progress => run_dir.join("progress.jsonl"),
                NativeAcpEventTarget::Trajectory | NativeAcpEventTarget::PostflightReceipt => {
                    trajectory_path.to_path_buf()
                }
            };
            append_trajectory_event(&target, event.payload.clone());
        }
    }
    append_trajectory_event(
        trajectory_path,
        json!({
            "event": "acp_native_turn_finished",
            "dispatch_id": dispatch_id,
            "agent": agent,
            "session_id": outcome.session_id,
            "agent_session_id": outcome.agent_session_id,
            "used_existing_session": outcome.used_existing_session,
            "mapped_events": outcome.mapped_events,
            "raw_stream": stream_path.to_string_lossy(),
            "process_exit_code": process_exit_code,
            "worker_mappings_published": publish_worker_mappings,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );
    Ok(())
}

fn persist_raw_stream(
    outcome: &NativeAcpPromptOutcome,
    run_dir: &Path,
) -> Result<std::path::PathBuf, String> {
    let stream_path = run_dir.join(ACP_STREAM_FILE);
    let lines = outcome
        .raw_messages
        .iter()
        .map(|message| {
            serde_json::to_string(message)
                .map_err(|err| format!("serialize ACP stream message: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let payload = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    crate::utils::write_owner_only_file_atomic(&stream_path, payload.as_bytes())
        .map_err(|err| format!("write {ACP_STREAM_FILE}: {err}"))?;
    Ok(stream_path)
}

#[cfg(test)]
mod auth_snapshot_tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn store(rows: &[(&str, Option<i64>, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE credential (id text PRIMARY KEY, \
                 integration_id text, label text NOT NULL, value text NOT NULL, connector_id text, \
                 method_id text, active integer, time_created integer NOT NULL, \
                 time_updated integer NOT NULL);",
            )
            .unwrap();
        for (index, (provider, active, value)) in rows.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO credential VALUES (?1, ?2, 'label', ?3, NULL, NULL, ?4, 0, 0)",
                    rusqlite::params![index.to_string(), provider, value, active],
                )
                .unwrap();
        }
        (dir, db)
    }

    fn snapshot(db: &Path, model: &str) -> Result<serde_json::Value, String> {
        opencode_db_auth_snapshot(db, model, 1200, NOW)
            .map(|body| serde_json::from_slice(&body.expect("credential store")).unwrap())
    }

    #[test]
    fn exports_only_the_pinned_provider_in_legacy_shape() {
        let oauth = format!(
            r#"{{"type":"oauth","methodID":"chatgpt-headless","refresh":"r","access":"a","expires":{},"metadata":{{"accountID":"acct"}}}}"#,
            (NOW + 86_400) * 1000
        );
        let (_dir, db) = store(&[
            ("openai", Some(1), &oauth),
            ("deepseek", Some(1), r#"{"type":"key","key":"k"}"#),
            ("google", Some(0), r#"{"type":"key","key":"old"}"#),
            ("google", Some(1), r#"{"type":"key","key":"new"}"#),
        ]);
        assert_eq!(
            snapshot(&db, "openai/gpt-6.1-sol").unwrap(),
            serde_json::json!({"openai": {
                "type": "oauth", "refresh": "r", "access": "a",
                "expires": (NOW + 86_400) * 1000, "accountId": "acct"
            }})
        );
        assert_eq!(
            snapshot(&db, "google/gemini").unwrap(),
            serde_json::json!({"google": {"type": "api", "key": "new"}})
        );
    }

    #[test]
    fn refuses_ambiguous_missing_or_expiring_credentials_without_leaking_material() {
        let expiring = format!(
            r#"{{"type":"oauth","refresh":"secret-r","access":"secret-a","expires":{}}}"#,
            (NOW + 1500) * 1000
        );
        let (_dir, db) = store(&[
            ("openai", Some(1), &expiring),
            ("deepseek", Some(1), r#"{"type":"key","key":"secret-1"}"#),
            ("deepseek", None, r#"{"type":"key","key":"secret-2"}"#),
        ]);
        for (model, expected) in [
            ("openai/gpt-6.1-sol", "expires before the run lifetime ends"),
            ("deepseek/v4", "several active deepseek credentials"),
            ("xai/grok", "no active xai credential"),
            ("gpt-6.1-sol", "no provider prefix"),
        ] {
            let error = snapshot(&db, model).unwrap_err();
            assert!(error.contains(expected), "{model}: {error}");
            assert!(!error.contains("secret"), "{model}: {error}");
        }
    }

    #[test]
    fn pre_credential_store_db_defers_to_auth_json_and_oversized_rows_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("opencode.db");
        rusqlite::Connection::open(&sessions)
            .unwrap()
            .execute_batch("CREATE TABLE session (id text PRIMARY KEY);")
            .unwrap();
        assert_eq!(
            opencode_db_auth_snapshot(&sessions, "openai/gpt-6.1-sol", 1200, NOW),
            Ok(None)
        );

        let huge = format!(
            r#"{{"type":"key","key":"{}"}}"#,
            "k".repeat(NATIVE_AUTH_SNAPSHOT_MAX_BYTES)
        );
        let (_dir, db) = store(&[("openai", Some(1), &huge)]);
        let error = snapshot(&db, "openai/gpt-6.1-sol").unwrap_err();
        assert!(error.contains("exceeds 1 MiB"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_removes_the_imported_credential_store() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "auth.json",
            "opencode.db",
            "opencode.db-wal",
            "opencode.db-shm",
            "log",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let mut cleanup = NativeAuthCleanup {
            parents: vec![std::fs::File::open(dir.path()).unwrap()],
        };
        cleanup.cleanup().unwrap();
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, ["log"]);
    }
}
