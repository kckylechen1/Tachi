//! Destination-local execution supervision and truthful lifecycle receipts
//! for Remote Execution v1 (A3 `review_revision` native review, A4 lifecycle &
//! cleanup facts, A5 `verify_pr` external acceptance adapter).

use super::admission::Connection;
use super::config::{open_verified_command_file, DestinationPolicy, GatewayConfig};
use super::profile::{JobKind, RemoteJob, StaticContext};
use crate::dispatch_ops::{run_remote_native_review, RemoteNativeReviewOutcome};
use crate::MemoryServer;
use memcore::db::a2a::{
    record_remote_task_cleanup, record_remote_task_started, record_remote_task_terminal,
    A2aTransitionActor, RemoteTask, RemoteTerminalOutcome,
};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{watch, OwnedRwLockReadGuard, RwLock};
use tokio::task::JoinHandle;

const LOG_TARGET: &str = "tachi::remote_a2a";
const MAX_ADAPTER_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_VERIFY_ENTRYPOINT_BYTES: u64 = 64 * 1024 * 1024;
const MACOS_IPC_BROKERS: &str = r#"(deny appleevent-send)(deny mach-lookup)(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo") (global-name "com.apple.FSEvents") (global-name "com.apple.SecurityServer") (global-name "com.apple.trustd.agent"))"#;
const KILL_GRACE_MS: u64 = 300;
const PGID_REAP_POLL_MS: u64 = 25;
const PGID_REAP_TIMEOUT_MS: u64 = 2_000;
const WORKTREE_ADD_TIMEOUT_SECS: u64 = 15;
const STDOUT_DRAIN_TIMEOUT_SECS: u64 = 2;
const LEASE_SAFETY_MARGIN_SECS: i64 = 2;
const DEFAULT_EXECUTION_TIMEOUT_SECS: u32 = 300;

#[cfg(unix)]
type ProcessGroupId = libc::pid_t;
#[cfg(not(unix))]
type ProcessGroupId = i32;

/// Tracks active destination runs so `CancelTask` signals the live supervisor
/// immediately and gateway shutdown drains child process groups cleanly.
#[derive(Default)]
pub(super) struct RemoteRunRegistry {
    closed: AtomicBool,
    admission_gate: Arc<RwLock<()>>,
    active: Mutex<HashMap<String, watch::Sender<bool>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl RemoteRunRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn begin_admission(&self) -> Option<OwnedRwLockReadGuard<()>> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let guard = self.admission_gate.clone().read_owned().await;
        (!self.closed.load(Ordering::Acquire)).then_some(guard)
    }

    pub fn register(&self, envelope_id: &str) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.insert(envelope_id.to_string(), tx);
        rx
    }

    pub fn signal_cancel(&self, envelope_id: &str) {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tx) = active.get(envelope_id) {
            let _ = tx.send(true);
        }
    }

    fn unregister(&self, envelope_id: &str) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.remove(envelope_id);
    }

    fn track(&self, handle: JoinHandle<()>) {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tasks.retain(|h| !h.is_finished());
        tasks.push(handle);
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        let _admission = self.admission_gate.write().await;
        {
            let active = self
                .active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for tx in active.values() {
                let _ = tx.send(true);
            }
        }
        let handles: Vec<JoinHandle<()>> = {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *tasks)
        };
        for handle in handles {
            let _ = handle.await;
        }
    }
}

fn pgid_dir(config: &GatewayConfig) -> PathBuf {
    config.canonical_home.join("run-pgids")
}

fn pgid_file(config: &GatewayConfig, local_run_id: &str) -> PathBuf {
    pgid_dir(config).join(format!("{local_run_id}.json"))
}

fn write_pgid_marker(
    config: &GatewayConfig,
    local_run_id: &str,
    run_root: &Path,
    mut payload: Value,
) -> Result<(), String> {
    create_private_dir(&pgid_dir(config))?;
    payload["run_root"] = json!(run_root.to_string_lossy());
    let bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
    crate::utils::write_owner_only_file_atomic(&pgid_file(config, local_run_id), &bytes)
}

fn remove_pgid_marker(config: &GatewayConfig, local_run_id: &str) {
    let _ = std::fs::remove_file(pgid_file(config, local_run_id));
}

#[cfg(target_os = "macos")]
fn current_boot_id() -> Result<String, String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let mut boot: libc::timeval = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&boot);
    // SAFETY: mib and boot are valid buffers of the specified lengths.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            (&mut boot as *mut libc::timeval).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(format!(
            "cannot read kernel boot time: {}",
            std::io::Error::last_os_error()
        ));
    }
    if len != std::mem::size_of_val(&boot) {
        return Err("kernel boot time has an unexpected size".into());
    }
    Ok(format!("{}:{}", boot.tv_sec, boot.tv_usec))
}

#[cfg(target_os = "linux")]
fn current_boot_id() -> Result<String, String> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|err| format!("cannot read kernel boot ID: {err}"))?;
    let boot = boot.trim();
    if boot.is_empty() {
        return Err("kernel boot ID is empty".into());
    }
    Ok(boot.to_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn current_boot_id() -> Result<String, String> {
    Err("kernel boot identity is unavailable".into())
}

#[cfg(target_os = "macos")]
fn process_start_token(pid: libc::pid_t) -> Option<String> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let len = std::mem::size_of_val(&info) as libc::c_int;
    // SAFETY: info is a writable proc_bsdinfo buffer of length len.
    let result = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            len,
        )
    };
    (result == len).then(|| format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[cfg(target_os = "linux")]
fn process_start_token(pid: libc::pid_t) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    Some(fields.split_whitespace().nth(19)?.to_owned())
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn process_start_token(_pid: libc::pid_t) -> Option<String> {
    None
}

fn mark_spawning(
    config: &GatewayConfig,
    run_id: &str,
    run_root: &Path,
    boot_id: &str,
) -> Result<(), String> {
    write_pgid_marker(
        config,
        run_id,
        run_root,
        json!({"state": "spawning", "boot_id": boot_id}),
    )
}

fn mark_running(
    config: &GatewayConfig,
    run_id: &str,
    run_root: &Path,
    pid: Option<u32>,
    boot_id: &str,
) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        let pgid = pid.ok_or("process_identity_unavailable")? as libc::pid_t;
        // macOS can stop reporting the start time as soon as the leader exits.
        // Keep the PGID so recovery can still check ESRCH without sending a signal.
        let token = process_start_token(pgid);
        write_pgid_marker(
            config,
            run_id,
            run_root,
            json!({"state": "running", "pgid": pgid, "boot_id": boot_id, "leader_start_token": token}),
        )
        .map_err(|_| "process_identity_unavailable")
    }
    #[cfg(not(unix))]
    {
        let _ = (config, run_id, run_root, pid, boot_id);
        Err("process_identity_unavailable")
    }
}

#[cfg(unix)]
fn process_group_is_gone(pgid: libc::pid_t) -> bool {
    // SAFETY: killpg(pgid, 0) checks whether any process remains in pgid.
    unsafe {
        libc::killpg(pgid, 0) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}

#[cfg(unix)]
fn kill_and_confirm_pgid_gone_blocking(pgid: libc::pid_t, leader_token: Option<&str>) -> bool {
    let my_pgid = unsafe { libc::getpgid(0) };
    if pgid <= 1 || pgid == my_pgid {
        return false;
    }
    if process_group_is_gone(pgid) {
        return true;
    }
    if !process_start_token(pgid).is_some_and(|token| leader_token == Some(token.as_str())) {
        return false;
    }
    // SAFETY: pgid is a dedicated worker process group distinct from our own.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(PGID_REAP_TIMEOUT_MS);
    while std::time::Instant::now() < deadline {
        if process_group_is_gone(pgid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(PGID_REAP_POLL_MS));
    }
    process_group_is_gone(pgid)
}

#[derive(serde::Deserialize)]
struct RunPgidMarker {
    state: String,
    run_root: Option<PathBuf>,
    pgid: Option<i32>,
    boot_id: Option<String>,
    #[cfg(unix)]
    leader_start_token: Option<String>,
}

fn recover_process_group(marker: &RunPgidMarker, boot_id: &str) -> Result<(), String> {
    if marker
        .boot_id
        .as_deref()
        .is_some_and(|recorded| recorded != boot_id)
    {
        return Ok(());
    }
    let pgid = marker
        .pgid
        .filter(|pgid| *pgid > 1)
        .ok_or("crashed mid-spawn without a verifiable PGID")?;
    #[cfg(unix)]
    {
        if process_group_is_gone(pgid) {
            return Ok(());
        }
        if marker.boot_id.as_deref() != Some(boot_id) {
            return Err(format!(
                "process group {pgid} has no recorded boot identity"
            ));
        }
        if kill_and_confirm_pgid_gone_blocking(pgid, marker.leader_start_token.as_deref()) {
            return Ok(());
        }
    }
    Err(format!(
        "process group {pgid} could not be authenticated and confirmed gone"
    ))
}

fn validate_recovery_run_root(
    config: &GatewayConfig,
    local_run_id: &str,
    run_root: &Path,
) -> Result<(), String> {
    let invalid = || format!("invalid recovery run_root for {local_run_id}");
    if !run_root.is_absolute()
        || run_root.file_name() != Some(OsStr::new(local_run_id))
        || run_root
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(invalid());
    }
    // A run can have crashed before its root was created or after it was removed.
    // Check the deepest existing ancestor and reject any intervening symlink.
    for ancestor in run_root.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                if ancestor.canonicalize().map_err(|_| invalid())? != ancestor {
                    return Err(invalid());
                }
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(invalid()),
        }
    }
    let overlaps = |other: &Path| {
        !other.as_os_str().is_empty()
            && (run_root.starts_with(other) || other.starts_with(run_root))
    };
    if overlaps(&config.canonical_home)
        || overlaps(&config.canonical_private_key)
        || overlaps(&config.canonical_config_path)
        || config.canonical_origin_db.as_deref().is_some_and(overlaps)
        || config
            .destinations
            .iter()
            .any(|dest| overlaps(&dest.canonical_repo_root))
    {
        return Err(invalid());
    }
    Ok(())
}

/// On gateway startup, verify any previously spawned worker process group is
/// dead before failing non-terminal runs or sweeping pending workspace cleanups.
/// Never relaunches an orphaned run and never releases a slot while an old
/// worker process group is still alive.
pub(super) fn recover_orphaned_gateway_runs(
    server: &MemoryServer,
    config: &GatewayConfig,
    gateway_actor: &A2aTransitionActor,
) -> Result<(), String> {
    let candidates: Vec<(String, String, String, String, String)> =
        server.with_global_store_read(|store| {
            let mut stmt = store
                .connection()
                .prepare(
                    "SELECT r.envelope_id, r.local_run_id, r.repository, r.lifecycle_state, r.cleanup_state
                     FROM a2a_remote_runs r
                     JOIN a2a_envelopes e ON e.envelope_id = r.envelope_id
                     WHERE e.recipient_agent_identity_id = ?1
                       AND (r.lifecycle_state IN ('admitted', 'started', 'cancel_requested')
                            OR r.cleanup_state = 'pending')
                     ORDER BY r.admitted_at, r.envelope_id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([&gateway_actor.agent_identity_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })?;

    for (envelope_id, local_run_id, repository, lifecycle_state, cleanup_state) in candidates {
        let marker_path = pgid_file(config, &local_run_id);
        let run_root = match std::fs::read_to_string(&marker_path) {
            Ok(raw) => {
                let marker: RunPgidMarker = serde_json::from_str(&raw).map_err(|_| {
                    format!("unreadable orphaned run pgid marker for {local_run_id}")
                })?;
                let run_root = marker
                    .run_root
                    .as_ref()
                    .ok_or_else(|| format!("missing recovery run_root for {local_run_id}"))?;
                validate_recovery_run_root(config, &local_run_id, run_root)?;
                match marker.state.as_str() {
                    "admitted_for_execution" | "reaped" | "terminal_persist_failed" => {}
                    "spawning" | "running" => {
                        current_boot_id()
                            .and_then(|boot_id| recover_process_group(&marker, &boot_id))
                            .map_err(|err| {
                                format!("orphaned run {local_run_id}: {err}; refusing to free slot or delete worktree")
                            })?;
                        write_pgid_marker(
                            config,
                            &local_run_id,
                            run_root,
                            json!({"state": "reaped"}),
                        )?;
                    }
                    _ => return Err(format!("unknown recovery marker state for {local_run_id}")),
                }
                run_root.clone()
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if lifecycle_state != "started" {
                    continue;
                }
                return Err(format!(
                    "missing recovery marker for started run {local_run_id}; refusing to guess run_root"
                ));
            }
            Err(err) => {
                return Err(format!(
                    "cannot read orphaned run pgid marker for {local_run_id}: {err}"
                ));
            }
        };

        if matches!(
            lifecycle_state.as_str(),
            "admitted" | "started" | "cancel_requested"
        ) {
            server.with_global_store(|store| {
                record_remote_task_terminal(
                    store.connection(),
                    gateway_actor,
                    &envelope_id,
                    RemoteTerminalOutcome::Failed,
                    None,
                    Some("gateway_restarted_run_orphaned"),
                    None,
                )
                .map_err(|e| e.to_string())
            })?;
        }

        if cleanup_state == "pending" {
            let dest = config
                .destinations
                .iter()
                .find(|d| d.repository == repository);
            let cleanup_err = cleanup_run_workspace(dest, &run_root).err();
            server.with_global_store(|store| {
                record_remote_task_cleanup(
                    store.connection(),
                    gateway_actor,
                    &envelope_id,
                    cleanup_err.is_none(),
                    cleanup_err,
                )
                .map_err(|e| e.to_string())
            })?;
        }
        remove_pgid_marker(config, &local_run_id);
    }
    Ok(())
}

struct AdmittedJobSnapshot {
    envelope_id: String,
    local_run_id: String,
    local_claim_id: String,
    repository: String,
    head_sha: String,
    job: JobKind,
    pull_request: Option<u32>,
    question: Option<String>,
    context: Option<StaticContext>,
    policy_profile: String,
}

pub(super) fn has_execution_marker(config: &GatewayConfig, local_run_id: &str) -> bool {
    pgid_file(config, local_run_id).exists()
}

pub(super) fn reconcile_unmarked_admitted_runs(
    conn: &rusqlite::Connection,
    config: &GatewayConfig,
    gateway_actor: &A2aTransitionActor,
) -> Result<(), memcore::MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT r.envelope_id, r.local_run_id
         FROM a2a_remote_runs r
         JOIN a2a_envelopes e ON e.envelope_id = r.envelope_id
         WHERE e.recipient_agent_identity_id = ?1
           AND r.lifecycle_state IN ('admitted', 'cancel_requested')
         ORDER BY r.admitted_at, r.envelope_id",
    )?;
    let rows = stmt
        .query_map([&gateway_actor.agent_identity_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for (envelope_id, local_run_id) in rows {
        if !has_execution_marker(config, &local_run_id) {
            let _ = memcore::db::a2a::reconcile_remote_run_if_expired(
                conn,
                gateway_actor,
                &envelope_id,
            )?;
        }
    }
    Ok(())
}

pub(super) fn mark_admitted_for_execution(
    config: &GatewayConfig,
    task: &RemoteTask,
) -> Result<(), memcore::MemoryError> {
    let run = task.run.as_ref().ok_or_else(|| {
        memcore::MemoryError::Internal("admitted execution is missing its run".into())
    })?;
    write_pgid_marker(
        config,
        &run.local_run_id,
        &remote_run_root(config, &run.local_run_id),
        json!({"state": "admitted_for_execution"}),
    )
    .map_err(memcore::MemoryError::Internal)
}

pub(super) fn spawn_admitted_run(connection: &Connection, task: &RemoteTask, job: &RemoteJob) {
    let Some(run) = task.run.as_ref() else {
        return;
    };
    let Some(dest) = connection
        .config
        .destinations
        .iter()
        .find(|d| d.repository == job.repository && d.allowed_jobs.contains(&job.job))
        .cloned()
    else {
        return;
    };
    let snapshot = AdmittedJobSnapshot {
        envelope_id: task.envelope.envelope_id.clone(),
        local_run_id: run.local_run_id.clone(),
        local_claim_id: run.local_claim_id.clone(),
        repository: job.repository.clone(),
        head_sha: job.head_sha.clone(),
        job: job.job,
        pull_request: job.pull_request,
        question: job.question.clone(),
        context: job.context.as_ref().map(|c| StaticContext {
            source_ref: c.source_ref.clone(),
            source_revision: c.source_revision.clone(),
            text: c.text.clone(),
            sha256: c.sha256.clone(),
        }),
        policy_profile: run.policy_profile.clone(),
    };
    let cancel_rx = connection.run_registry.register(&snapshot.envelope_id);
    let server = connection.server.clone();
    let config = connection.config.clone();
    let gateway_actor = connection.gateway_actor.clone();
    let registry = connection.run_registry.clone();
    let handle = tokio::spawn(async move {
        let envelope_id = snapshot.envelope_id.clone();
        execute_supervised_run(server, config, gateway_actor, dest, snapshot, cancel_rx).await;
        registry.unregister(&envelope_id);
    });
    connection.run_registry.track(handle);
}

fn remote_run_root(config: &GatewayConfig, local_run_id: &str) -> PathBuf {
    config
        .canonical_worker_runs_dir
        .as_deref()
        .unwrap_or(&config.canonical_home)
        .join(local_run_id)
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("create dir {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod 0700 {}: {e}", path.display()))?;
    }
    Ok(())
}

async fn prepare_isolated_worktree(
    config: &GatewayConfig,
    run_id: &str,
    dest: &DestinationPolicy,
    run_root: &Path,
    head_sha: &str,
    deadline: tokio::time::Instant,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<(PathBuf, PathBuf), (&'static str, bool)> {
    const PREPARE_FAILED: (&str, bool) = ("workspace_prepare_failed", true);
    if let Some(parent) = run_root.parent() {
        create_private_dir(parent).map_err(|_| PREPARE_FAILED)?;
    }
    create_private_dir(run_root).map_err(|_| PREPARE_FAILED)?;
    let runtime_dir = run_root.join("runtime");
    create_private_dir(&runtime_dir).map_err(|_| PREPARE_FAILED)?;
    for sub in ["home", "tmp", "config", "cache", "data"] {
        create_private_dir(&runtime_dir.join(sub)).map_err(|_| PREPARE_FAILED)?;
    }
    let worktree = run_root.join("worktree");
    let (canonical_root, canonical_git_dir) =
        super::config::resolve_pinned_git_dir(&dest.repo_root).map_err(|_| PREPARE_FAILED)?;
    if canonical_root != dest.canonical_repo_root || canonical_git_dir != dest.canonical_git_dir {
        return Err(PREPARE_FAILED);
    }

    let mut cmd = Command::new("git");
    cmd.env_clear()
        .env("PATH", sanitized_worker_path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .arg("--git-dir")
        .arg(&canonical_git_dir)
        .arg("--work-tree")
        .arg(&canonical_root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.allow=never",
            "-c",
            "submodule.recurse=false",
            "-c",
            "advice.detachedHead=false",
            "worktree",
            "add",
            "--detach",
        ])
        .arg(&worktree)
        .arg(head_sha)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(parent) = canonical_root.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    #[cfg(unix)]
    cmd.process_group(0);

    let boot_id = current_boot_id().map_err(|_| PREPARE_FAILED)?;
    mark_spawning(config, run_id, run_root, &boot_id).map_err(|_| PREPARE_FAILED)?;
    let mut child = cmd.spawn().map_err(|_| PREPARE_FAILED)?;
    let pgid = child.id().map(|pid| pid as ProcessGroupId);
    if mark_running(config, run_id, run_root, child.id(), &boot_id).is_err() {
        let reaped = terminate_and_reap(&mut child, pgid).await;
        return Err(("process_identity_unavailable", reaped));
    }
    let git_deadline =
        deadline.min(tokio::time::Instant::now() + Duration::from_secs(WORKTREE_ADD_TIMEOUT_SECS));
    let exit_observed = tokio::select! {
        result = wait_for_run_leader(&mut child, pgid, git_deadline) => result,
        changed = cancel_rx.changed() => {
            let reaped = terminate_and_reap(&mut child, pgid).await;
            if changed.is_ok() && *cancel_rx.borrow() {
                return Err(("canceled_before_spawn", reaped));
            }
            return Err(("workspace_prepare_failed", reaped));
        }
    };
    if !terminate_and_reap(&mut child, pgid).await {
        return Err(("process_group_still_alive", false));
    }
    write_pgid_marker(config, run_id, run_root, json!({"state": "reaped"}))
        .map_err(|_| PREPARE_FAILED)?;
    match exit_observed {
        Ok(false) => return Err(("workspace_prepare_timeout", true)),
        Ok(true) if child.wait().await.is_ok_and(|status| status.success()) => {}
        _ => return Err(PREPARE_FAILED),
    }
    let canon_worktree = std::fs::canonicalize(&worktree).map_err(|_| PREPARE_FAILED)?;
    let canon_runtime = std::fs::canonicalize(&runtime_dir).map_err(|_| PREPARE_FAILED)?;
    Ok((canon_worktree, canon_runtime))
}

fn cleanup_run_workspace(
    dest: Option<&DestinationPolicy>,
    run_root: &Path,
) -> Result<(), &'static str> {
    let worktree = run_root.join("worktree");
    if worktree.exists() {
        if let Some(dest) = dest {
            if let Some(mut git) = super::rpc::pinned_git_command(dest) {
                let _ = git
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .args([
                        "-c",
                        "core.hooksPath=/dev/null",
                        "worktree",
                        "remove",
                        "--force",
                    ])
                    .arg(&worktree)
                    .output();
            }
            if let Some(mut git) = super::rpc::pinned_git_command(dest) {
                let _ = git
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .args(["-c", "core.hooksPath=/dev/null", "worktree", "prune"])
                    .output();
            }
        }
    }
    if run_root.exists() {
        std::fs::remove_dir_all(run_root).map_err(|_| "workspace_cleanup_failed")?;
    }
    Ok(())
}

/// Checks start-window expiry, non-revoked sender mTLS admission, and exact
/// local WorkClaim holder/version/project/lease before creating any worktree
/// or spawning any worker. Returns the active local claim's `lease_expires_at`.
fn precheck_launch_authority(
    server: &MemoryServer,
    gateway_actor: &A2aTransitionActor,
    envelope_id: &str,
) -> Result<jiff::Timestamp, &'static str> {
    struct LaunchAuthorityRow {
        issuer_id: String,
        recipient_id: String,
        admission_id: String,
        trust_domain: String,
        state: String,
        expires_at: String,
        claim_id: String,
        claim_version: i64,
        canonical_project: String,
    }
    const UNAUTHORIZED: &str = "launch_authority_expired_or_revoked";
    let result = server.with_global_store_read(|store| {
        let conn = store.connection();
        if !memcore::db::a2a::a2a_actor_is_current(conn, gateway_actor)
            .map_err(|e| e.to_string())?
        {
            return Ok(Err(UNAUTHORIZED));
        }
        let row = conn
            .query_row(
                "SELECT e.issuer_agent_identity_id, e.issuer_admission_id, e.issuer_trust_domain,
                        e.current_state, e.expires_at, r.local_claim_id, r.local_claim_version,
                        r.canonical_project, e.recipient_agent_identity_id
                 FROM a2a_envelopes e
                 JOIN a2a_remote_runs r ON r.envelope_id = e.envelope_id
                 WHERE e.envelope_id = ?1 AND e.kind = 'tachi.remote_job/v1'",
                [envelope_id],
                |row| {
                    Ok(LaunchAuthorityRow {
                        issuer_id: row.get(0)?,
                        admission_id: row.get(1)?,
                        trust_domain: row.get(2)?,
                        state: row.get(3)?,
                        expires_at: row.get(4)?,
                        claim_id: row.get(5)?,
                        claim_version: row.get(6)?,
                        canonical_project: row.get(7)?,
                        recipient_id: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(LaunchAuthorityRow {
            issuer_id,
            recipient_id,
            admission_id,
            trust_domain,
            state,
            expires_at,
            claim_id,
            claim_version,
            canonical_project,
        }) = row
        else {
            return Ok(Err(UNAUTHORIZED));
        };
        if recipient_id != gateway_actor.agent_identity_id {
            return Ok(Err(UNAUTHORIZED));
        }
        match state.as_str() {
            "remote_admitted" => {}
            "remote_cancel_requested" => return Ok(Err("canceled_before_spawn")),
            _ => return Ok(Err(UNAUTHORIZED)),
        }
        let now = jiff::Timestamp::now();
        if expires_at
            .parse::<jiff::Timestamp>()
            .map_or(true, |exp| exp <= now)
        {
            return Ok(Err(UNAUTHORIZED));
        }
        let Some(receipt) = memcore::get_verified_admission_receipt(conn, &admission_id)
            .map_err(|e| e.to_string())?
        else {
            return Ok(Err(UNAUTHORIZED));
        };
        if receipt.current_state != "verified"
            || receipt.agent_identity_id != issuer_id
            || receipt.trust_domain != trust_domain
            || receipt.verification_method != memcore::VERIFIED_ADMISSION_MTLS_METHOD
            || receipt
                .evidence_expires_at
                .parse::<jiff::Timestamp>()
                .map_or(true, |exp| exp <= now)
            || memcore::verified_mtls_key_is_revoked(
                conn,
                &receipt.issuer_id,
                &receipt.evidence_ref,
            )
            .map_err(|e| e.to_string())?
        {
            return Ok(Err(UNAUTHORIZED));
        }
        let Some(claim) = memcore::get_claim(conn, &claim_id).map_err(|e| e.to_string())? else {
            return Ok(Err(UNAUTHORIZED));
        };
        let claim_project: Option<String> = conn
            .query_row(
                "SELECT canonical_project FROM session_claims WHERE claim_id = ?1",
                [&claim_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if claim.state != memcore::ClaimState::Active
            || claim.transition_version != claim_version
            || claim.agent_identity_id.as_deref() != Some(gateway_actor.agent_identity_id.as_str())
            || claim_project.as_deref() != Some(canonical_project.as_str())
        {
            return Ok(Err(UNAUTHORIZED));
        }
        let Some(lease_exp) = claim
            .lease_expires_at
            .as_deref()
            .and_then(|ts| ts.parse::<jiff::Timestamp>().ok())
        else {
            return Ok(Err(UNAUTHORIZED));
        };
        if lease_exp <= now + jiff::SignedDuration::from_secs(LEASE_SAFETY_MARGIN_SECS + 1) {
            return Ok(Err(UNAUTHORIZED));
        }
        Ok(Ok(lease_exp))
    });
    result.map_err(|_| UNAUTHORIZED)?
}

fn sbpl_escape(path: &Path) -> Result<String, &'static str> {
    let text = path.to_str().ok_or("sandbox_unavailable")?;
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' | '\r' | '\0' => return Err("sandbox_unavailable"),
            other => out.push(other),
        }
    }
    out.push('"');
    Ok(out)
}

fn gateway_authority_files(config: &GatewayConfig) -> Vec<PathBuf> {
    let mut files = vec![
        config.canonical_private_key.clone(),
        config.canonical_config_path.clone(),
    ];
    if let Some(origin_db) = &config.canonical_origin_db {
        files.push(origin_db.clone());
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = origin_db.as_os_str().to_os_string();
            sidecar.push(suffix);
            files.push(sidecar.into());
        }
    }
    files.retain(|path| !path.as_os_str().is_empty());
    files
}

/// Wraps a remote worker invocation (`review_revision` or `verify_pr`) in the
/// host kernel sandbox (`sandbox-exec` on macOS, `bwrap` on Linux) denying all
/// writes outside `runtime_dir` AND denying all reads/writes to gateway
/// authority files (`canonical_home`, `canonical_private_key`,
/// `canonical_config_path`, `canonical_origin_db`).
fn build_sandboxed_worker_command(
    config: &GatewayConfig,
    program: &OsStr,
    args: &[OsString],
    worktree: &Path,
    runtime_dir: &Path,
    verified_bin_dir: Option<&Path>,
) -> Result<Command, &'static str> {
    if !tachi_dispatch::native_sandbox_available() {
        return Err("sandbox_unavailable");
    }
    let worker_runs_dir = config
        .canonical_worker_runs_dir
        .as_deref()
        .ok_or("sandbox_unavailable")?;
    let home = runtime_dir.join("home");
    let tmp = runtime_dir.join("tmp");
    let xdg_config = runtime_dir.join("config");
    let xdg_cache = runtime_dir.join("cache");
    let xdg_data = runtime_dir.join("data");

    let mut operator_paths = config
        .worker_credential_paths()
        .map_err(|_| "sandbox_unavailable")?;
    let authority_files = gateway_authority_files(config);
    let mut cmd = match std::env::consts::OS {
        "macos" => {
            let mut operator_clauses = String::new();
            for (path, is_dir) in operator_paths {
                let filter = if is_dir { "subpath" } else { "literal" };
                operator_clauses.push_str(&format!(" ({filter} {})", sbpl_escape(&path)?));
            }
            let operator_denial = if operator_clauses.is_empty() {
                String::new()
            } else {
                format!("(deny file-read* file-write*{operator_clauses})")
            };
            let mut authority_clauses =
                format!("(subpath {})", sbpl_escape(&config.canonical_home)?);
            for path in authority_files {
                authority_clauses.push_str(&format!(" (literal {})", sbpl_escape(&path)?));
            }
            // Seatbelt applies the final matching rule.
            let runtime_scope = sbpl_escape(runtime_dir)?;
            let worker_runs_scope = sbpl_escape(worker_runs_dir)?;
            let mut read_scopes = format!("(subpath {})", sbpl_escape(worktree)?);
            if let Some(dir) = verified_bin_dir {
                read_scopes.push_str(&format!(" (subpath {})", sbpl_escape(dir)?));
            }
            let policy = format!(
                r#"(version 1)(allow default)(deny file-write*)(deny process-info-setcontrol)
                 (allow file-write* (literal "/dev/null") (literal "/dev/zero") (literal "/dev/stdout") (literal "/dev/stderr") (subpath "/dev/fd") (literal "/dev/dtracehelper"))
                 {MACOS_IPC_BROKERS}{operator_denial}
                 (deny file-read* (subpath {worker_runs_scope}))
                 (allow file-read* {read_scopes})
                 (allow file-read* file-write* (subpath {runtime_scope}))
                 (deny file-read* file-write* {authority_clauses})"#,
            );
            let mut c = Command::new("/usr/bin/sandbox-exec");
            c.arg("-p").arg(policy).arg(program).args(args);
            c
        }
        "linux" => {
            let mut c = Command::new("/usr/bin/bwrap");
            c.args([
                "--unshare-user",
                "--unshare-pid",
                "--unshare-ipc",
                "--unshare-uts",
                "--disable-userns",
                "--cap-drop",
                "ALL",
                "--die-with-parent",
                "--ro-bind",
                "/",
                "/",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
            ]);
            operator_paths.insert(config.canonical_home.clone(), true);
            operator_paths.extend(authority_files.into_iter().map(|path| (path, false)));
            let mut masks = std::collections::BTreeMap::new();
            for path in operator_paths.into_keys() {
                let canonical = match path.canonicalize() {
                    Ok(path) => path,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => return Err("sandbox_unavailable"),
                };
                let metadata = std::fs::metadata(&canonical).map_err(|_| "sandbox_unavailable")?;
                if metadata.is_dir() || metadata.is_file() {
                    masks.insert(canonical, metadata.is_dir());
                }
            }
            let mut masked_dirs: Vec<PathBuf> = Vec::new();
            for (path, is_dir) in masks {
                if masked_dirs.iter().any(|dir| path.starts_with(dir)) {
                    continue;
                }
                if is_dir {
                    c.arg("--tmpfs").arg(&path);
                    masked_dirs.push(path);
                } else {
                    c.args(["--ro-bind", "/dev/null"]).arg(path);
                }
            }
            c.arg("--tmpfs").arg(worker_runs_dir);
            c.arg("--bind")
                .arg(runtime_dir)
                .arg(runtime_dir)
                .arg("--ro-bind")
                .arg(worktree)
                .arg(worktree);
            if let Some(dir) = verified_bin_dir {
                c.arg("--ro-bind").arg(dir).arg(dir);
            }
            c.arg("--chdir")
                .arg(worktree)
                .arg("--")
                .arg(program)
                .args(args);
            c
        }
        _ => return Err("sandbox_unavailable"),
    };

    cmd.current_dir(worktree)
        .env_clear()
        .env("PATH", sanitized_worker_path())
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("LANG", "C.UTF-8")
        .env("NO_COLOR", "1");
    for key in [
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "NO_PROXY",
    ] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    Ok(cmd)
}

struct TerminalVerdict {
    outcome: RemoteTerminalOutcome,
    summary_digest: Option<String>,
    error_class: Option<&'static str>,
    external_ref: Option<String>,
    reaped: bool,
}

async fn execute_supervised_run(
    server: MemoryServer,
    config: Arc<GatewayConfig>,
    gateway_actor: A2aTransitionActor,
    dest: DestinationPolicy,
    job: AdmittedJobSnapshot,
    mut cancel_rx: watch::Receiver<bool>,
) {
    let run_root = remote_run_root(&config, &job.local_run_id);

    let verdict = if *cancel_rx.borrow() {
        TerminalVerdict {
            outcome: RemoteTerminalOutcome::Canceled,
            summary_digest: None,
            error_class: None,
            external_ref: None,
            reaped: true,
        }
    } else {
        match precheck_launch_authority(&server, &gateway_actor, &job.envelope_id) {
            Err("canceled_before_spawn") => TerminalVerdict {
                outcome: RemoteTerminalOutcome::Canceled,
                summary_digest: None,
                error_class: None,
                external_ref: None,
                reaped: true,
            },
            Err(err_class) => TerminalVerdict {
                outcome: RemoteTerminalOutcome::Failed,
                summary_digest: None,
                error_class: Some(err_class),
                external_ref: None,
                reaped: true,
            },
            Ok(lease_expires_at) => {
                // P1 #10: cap execution deadline by remaining local claim lease minus margin.
                let now = jiff::Timestamp::now();
                let remaining_secs =
                    (lease_expires_at.as_second() - now.as_second() - LEASE_SAFETY_MARGIN_SECS)
                        .max(1) as u64;
                let configured_secs = u64::from(match job.job {
                    JobKind::ReviewRevision => dest
                        .review_timeout_secs
                        .unwrap_or(dest.lease_ttl_secs.min(DEFAULT_EXECUTION_TIMEOUT_SECS)),
                    JobKind::VerifyPr => dest
                        .verify_timeout_secs
                        .unwrap_or(dest.lease_ttl_secs.min(DEFAULT_EXECUTION_TIMEOUT_SECS)),
                });
                let deadline = tokio::time::Instant::now()
                    + Duration::from_secs(configured_secs.min(remaining_secs));

                match prepare_isolated_worktree(
                    &config,
                    &job.local_run_id,
                    &dest,
                    &run_root,
                    &job.head_sha,
                    deadline,
                    &mut cancel_rx,
                )
                .await
                {
                    Err(("canceled_before_spawn", reaped)) => TerminalVerdict {
                        outcome: RemoteTerminalOutcome::Canceled,
                        summary_digest: None,
                        error_class: None,
                        external_ref: None,
                        reaped,
                    },
                    Err((err_class, reaped)) => TerminalVerdict {
                        outcome: RemoteTerminalOutcome::Failed,
                        summary_digest: None,
                        error_class: Some(err_class),
                        external_ref: None,
                        reaped,
                    },
                    Ok((worktree, runtime_dir)) => {
                        if *cancel_rx.borrow() {
                            TerminalVerdict {
                                outcome: RemoteTerminalOutcome::Canceled,
                                summary_digest: None,
                                error_class: None,
                                external_ref: None,
                                reaped: true,
                            }
                        } else {
                            match job.job {
                                JobKind::ReviewRevision => {
                                    run_native_review(
                                        &server,
                                        &config,
                                        &gateway_actor,
                                        &job,
                                        &run_root,
                                        &worktree,
                                        &runtime_dir,
                                        deadline,
                                        &mut cancel_rx,
                                    )
                                    .await
                                }
                                JobKind::VerifyPr => {
                                    run_verify_pr(
                                        &server,
                                        &config,
                                        &gateway_actor,
                                        &dest,
                                        &job,
                                        &run_root,
                                        &worktree,
                                        &runtime_dir,
                                        deadline,
                                        &mut cancel_rx,
                                    )
                                    .await
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    if !verdict.reaped {
        tracing::error!(
            target: LOG_TARGET,
            run_id = %job.local_run_id,
            error_class = ?verdict.error_class,
            "remote run process group was not confirmed reaped"
        );
        return;
    }
    if let Err(error) = write_pgid_marker(
        &config,
        &job.local_run_id,
        &run_root,
        json!({"state": "reaped"}),
    ) {
        tracing::error!(target: LOG_TARGET, run_id = %job.local_run_id, %error, "cannot persist process reaping marker");
    }

    let envelope_id = job.envelope_id.clone();
    let actor_clone = gateway_actor.clone();
    let server_clone = server.clone();
    let terminal = tokio::task::spawn_blocking(move || {
        let mut backoffs = [50, 100, 200, 400, 800].into_iter();
        loop {
            let result = server_clone.with_global_store(|store| {
                Ok(record_remote_task_terminal(
                    store.connection(),
                    &actor_clone,
                    &envelope_id,
                    verdict.outcome,
                    verdict.summary_digest.as_deref(),
                    verdict.error_class,
                    verdict.external_ref.as_deref(),
                ))
            });
            let error = match result {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(error)) => error,
                Err(error) => return Err(error),
            };
            let transient = matches!(
                &error,
                memcore::MemoryError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
                    if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ) || matches!(&error, memcore::MemoryError::WorkClaimConflict(_));
            match backoffs.next().filter(|_| transient) {
                Some(delay) => std::thread::sleep(Duration::from_millis(delay)),
                None => return Err(error.to_string()),
            }
        }
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);

    if let Err(error) = terminal {
        tracing::error!(target: LOG_TARGET, run_id = %job.local_run_id, %error, "remote terminal receipt could not be persisted");
        if let Err(error) = write_pgid_marker(
            &config,
            &job.local_run_id,
            &run_root,
            json!({"state": "terminal_persist_failed"}),
        ) {
            tracing::error!(target: LOG_TARGET, run_id = %job.local_run_id, %error, "cannot update terminal failure marker");
        }
        return;
    }

    let cleanup = tokio::task::spawn_blocking(move || {
        let cleanup_err = cleanup_run_workspace(Some(&dest), &run_root).err();
        server.with_global_store(|store| {
            record_remote_task_cleanup(
                store.connection(),
                &gateway_actor,
                &job.envelope_id,
                cleanup_err.is_none(),
                cleanup_err,
            )
            .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);
    match cleanup {
        Ok(run) => remove_pgid_marker(&config, &run.local_run_id),
        Err(error) => {
            tracing::error!(target: LOG_TARGET, %error, "remote cleanup receipt could not be persisted")
        }
    }
}

fn sanitized_worker_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string())
}

fn build_review_prompt(job: &AdmittedJobSnapshot) -> String {
    let mut prompt = format!(
        "Read-only remote code review.\nRepository: {}\nCommit: {}\n",
        job.repository, job.head_sha
    );
    if let Some(question) = &job.question {
        prompt.push_str("\nReview Question:\n");
        prompt.push_str(question);
        prompt.push('\n');
    }
    if let Some(ctx) = &job.context {
        prompt.push_str(&format!(
            "\nStatic Context (source_ref={}, source_revision={}, sha256={}):\n{}\n",
            ctx.source_ref, ctx.source_revision, ctx.sha256, ctx.text
        ));
    }
    prompt
}

async fn wait_for_run_leader(
    child: &mut Child,
    pgid: Option<ProcessGroupId>,
    deadline: tokio::time::Instant,
) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        let _ = child;
        let pid = pgid.ok_or_else(|| std::io::Error::other("process group is unavailable"))?;
        crate::dispatch_ops::subprocess::wait_for_owned_root_exit(
            Some(pid as u32),
            deadline.saturating_duration_since(tokio::time::Instant::now()),
        )
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(result) => result.map(|_| true),
            Err(_) => Ok(false),
        }
    }
}

/// Signals the group while the child still reserves its PID, then proves ESRCH.
async fn terminate_and_reap(child: &mut Child, pgid: Option<ProcessGroupId>) -> bool {
    #[cfg(unix)]
    if let Some(pgid) = pgid {
        if child.id().is_some() && !process_group_is_gone(pgid) {
            // SAFETY: this still-owned child was spawned with process_group(0).
            unsafe {
                libc::killpg(pgid, libc::SIGTERM);
            }
            let grace_deadline = tokio::time::Instant::now() + Duration::from_millis(KILL_GRACE_MS);
            let _ = wait_for_run_leader(child, Some(pgid), grace_deadline).await;
            if !process_group_is_gone(pgid) {
                unsafe {
                    libc::killpg(pgid, libc::SIGKILL);
                }
            }
        }
    }
    let _ = child.start_kill();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(PGID_REAP_TIMEOUT_MS);
    if !tokio::time::timeout_at(deadline, child.wait())
        .await
        .is_ok_and(|result| result.is_ok())
    {
        return false;
    }
    #[cfg(unix)]
    if let Some(pgid) = pgid {
        while tokio::time::Instant::now() < deadline {
            if process_group_is_gone(pgid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(PGID_REAP_POLL_MS)).await;
        }
        return process_group_is_gone(pgid);
    }
    #[cfg(not(unix))]
    let _ = pgid;
    cfg!(not(unix))
}

async fn record_spawned_worker_start(
    server: &MemoryServer,
    gateway_actor: &A2aTransitionActor,
    envelope_id: &str,
    external_ref: Option<String>,
) -> Result<(), &'static str> {
    let server = server.clone();
    let gateway_actor = gateway_actor.clone();
    let envelope_id = envelope_id.to_owned();
    tokio::task::spawn_blocking(move || {
        server.with_global_store(|store| {
            record_remote_task_started(
                store.connection(),
                &gateway_actor,
                &envelope_id,
                external_ref.as_deref(),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|_| "start_boundary_rejected")?
    .map_err(|_| "start_boundary_rejected")
}

async fn authorize_worker_start(
    server: &MemoryServer,
    gateway_actor: &A2aTransitionActor,
    envelope_id: &str,
) -> Result<(), &'static str> {
    let server = server.clone();
    let gateway_actor = gateway_actor.clone();
    let envelope_id = envelope_id.to_owned();
    tokio::task::spawn_blocking(move || {
        precheck_launch_authority(&server, &gateway_actor, &envelope_id).map(|_| ())
    })
    .await
    .map_err(|_| "start_boundary_rejected")?
}

#[allow(clippy::too_many_arguments)]
async fn run_native_review(
    server: &MemoryServer,
    config: &Arc<GatewayConfig>,
    gateway_actor: &A2aTransitionActor,
    job: &AdmittedJobSnapshot,
    run_root: &Path,
    worktree: &Path,
    runtime_dir: &Path,
    deadline: tokio::time::Instant,
    cancel_rx: &mut watch::Receiver<bool>,
) -> TerminalVerdict {
    let Some(profile) = tachi_dispatch::native_session_profile(&job.policy_profile) else {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("invalid_review_profile"),
            external_ref: None,
            reaped: true,
        };
    };
    if profile.writable {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("invalid_review_profile"),
            external_ref: None,
            reaped: true,
        };
    }
    let Ok(mut invocation) = tachi_dispatch::native_session_command(&job.policy_profile) else {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("adapter_unavailable"),
            external_ref: None,
            reaped: true,
        };
    };
    let program = invocation.remove(0);
    let args: Vec<OsString> = invocation.into_iter().map(OsString::from).collect();
    let mut cmd = match build_sandboxed_worker_command(
        config,
        OsStr::new(&program),
        &args,
        worktree,
        runtime_dir,
        None,
    ) {
        Ok(cmd) => cmd,
        Err(err_class) => {
            return TerminalVerdict {
                outcome: RemoteTerminalOutcome::Failed,
                summary_digest: None,
                error_class: Some(err_class),
                external_ref: None,
                reaped: true,
            };
        }
    };
    cmd.env("TACHI_REMOTE_JOB", "review_revision")
        .env("TACHI_REMOTE_REPOSITORY", &job.repository)
        .env("TACHI_REMOTE_HEAD_SHA", &job.head_sha)
        .env("TACHI_REMOTE_LOCAL_RUN_ID", &job.local_run_id);

    let Ok(boot_id) = current_boot_id() else {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("spawn_failed"),
            external_ref: None,
            reaped: true,
        };
    };
    let ext_ref = format!("native:{}:{}", profile.backend, job.local_run_id);

    let spawn_boot_id = boot_id.as_str();
    let spawn_ext_ref = ext_ref.as_str();
    let outcome = run_remote_native_review(
        &job.policy_profile,
        cmd,
        worktree,
        run_root,
        runtime_dir,
        &job.local_run_id,
        build_review_prompt(job),
        deadline,
        cancel_rx,
        || async {
            authorize_worker_start(server, gateway_actor, &job.envelope_id).await?;
            mark_spawning(config, &job.local_run_id, run_root, &boot_id).map_err(|_| "spawn_failed")
        },
        |pid| async move {
            mark_running(
                config,
                &job.local_run_id,
                run_root,
                Some(pid),
                spawn_boot_id,
            )?;
            record_spawned_worker_start(
                server,
                gateway_actor,
                &job.envelope_id,
                Some(spawn_ext_ref.to_owned()),
            )
            .await
        },
    )
    .await;

    match outcome {
        RemoteNativeReviewOutcome::Completed { output, reaped } => {
            let digest = format!("{:x}", Sha256::digest(output.as_bytes()));
            TerminalVerdict {
                outcome: RemoteTerminalOutcome::Completed,
                summary_digest: Some(digest),
                error_class: None,
                external_ref: Some(ext_ref),
                reaped,
            }
        }
        RemoteNativeReviewOutcome::Canceled { reaped } => TerminalVerdict {
            outcome: RemoteTerminalOutcome::Canceled,
            summary_digest: None,
            error_class: None,
            external_ref: Some(ext_ref),
            reaped,
        },
        RemoteNativeReviewOutcome::Failed {
            error_class: "canceled_before_spawn",
            reaped,
        } => TerminalVerdict {
            outcome: RemoteTerminalOutcome::Canceled,
            summary_digest: None,
            error_class: None,
            external_ref: None,
            reaped,
        },
        RemoteNativeReviewOutcome::Failed {
            error_class,
            reaped,
        } => TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some(error_class),
            external_ref: if matches!(
                error_class,
                "provider_auth_unavailable" | "spawn_failed" | "start_boundary_rejected"
            ) {
                None
            } else {
                Some(ext_ref)
            },
            reaped,
        },
    }
}

fn is_valid_github_receipt_fragment(fragment: &str) -> bool {
    for prefix in ["issuecomment-", "pullrequestreview-", "discussion_r"] {
        if let Some(digits) = fragment.strip_prefix(prefix) {
            return !digits.is_empty()
                && digits.len() <= 24
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !digits.starts_with('0');
        }
    }
    false
}

fn is_valid_github_check_query(query: &str) -> bool {
    let Some(digits) = query.strip_prefix("check_run_id=") else {
        return false;
    };
    !digits.is_empty()
        && digits.len() <= 24
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0')
}

/// Validates the external `verify_pr` acceptance lifecycle receipt.
/// GitHub carries the authoritative acceptance record; a worker's exit 0
/// without an exact-head GitHub PR comment/check receipt URL and exact
/// repository binding is never promoted to `completed`.
pub(super) fn validate_verify_pr_receipt(
    stdout: &[u8],
    repository: &str,
    pull_request: u32,
    head_sha: &str,
) -> Result<(RemoteTerminalOutcome, String, String), &'static str> {
    let parsed: Value = serde_json::from_slice(stdout).map_err(|_| "invalid_external_receipt")?;
    let Some(obj) = parsed.as_object() else {
        return Err("invalid_external_receipt");
    };
    let status = obj
        .get("status")
        .and_then(Value::as_str)
        .ok_or("invalid_external_receipt")?;
    let outcome = match status {
        "completed" => RemoteTerminalOutcome::Completed,
        "failed" => RemoteTerminalOutcome::Failed,
        _ => return Err("invalid_external_receipt"),
    };
    if obj.get("repository").and_then(Value::as_str) != Some(repository) {
        return Err("invalid_external_receipt");
    }
    if obj.get("head_sha").and_then(Value::as_str) != Some(head_sha) {
        return Err("invalid_external_receipt");
    }
    if obj.get("pull_request").and_then(Value::as_u64) != Some(u64::from(pull_request)) {
        return Err("invalid_external_receipt");
    }
    let receipt_url = obj
        .get("receipt_url")
        .and_then(Value::as_str)
        .ok_or("invalid_external_receipt")?;
    if receipt_url.len() > 512
        || receipt_url.chars().any(char::is_whitespace)
        || tachi_runtime_core::secret_scan::contains_secret_like(receipt_url)
    {
        return Err("invalid_external_receipt");
    }
    let parsed_url = url::Url::parse(receipt_url).map_err(|_| "invalid_external_receipt")?;
    let expected_pr_path = format!("/{repository}/pull/{pull_request}");
    let expected_checks_path = format!("{expected_pr_path}/checks");
    if parsed_url.scheme() != "https"
        || parsed_url.host_str() != Some("github.com")
        || !parsed_url.username().is_empty()
        || parsed_url.password().is_some()
        || parsed_url.port().is_some()
        || !receipt_url.starts_with("https://github.com/")
    {
        return Err("invalid_external_receipt");
    }
    let valid_comment = parsed_url.path() == expected_pr_path
        && parsed_url.query().is_none()
        && parsed_url
            .fragment()
            .is_some_and(is_valid_github_receipt_fragment);
    let valid_check_run = parsed_url.path() == expected_checks_path
        && parsed_url.fragment().is_none()
        && parsed_url.query().is_some_and(is_valid_github_check_query);
    if !(valid_comment || valid_check_run) {
        return Err("invalid_external_receipt");
    }
    let digest = format!("{:x}", Sha256::digest(stdout));
    Ok((outcome, receipt_url.to_string(), digest))
}

fn stage_verify_entrypoint(
    mut pinned_exe_file: std::fs::File,
    run_root: &Path,
) -> std::io::Result<PathBuf> {
    let verified_bin = run_root.join("verified-bin");
    let mut dir = std::fs::DirBuilder::new();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        dir.mode(0o700);
        options.mode(0o500).custom_flags(libc::O_NOFOLLOW);
    }
    dir.create(&verified_bin)?;
    let entrypoint = verified_bin.join("verify-entrypoint");
    let mut destination = options.open(&entrypoint)?;
    std::io::copy(
        &mut (&mut pinned_exe_file).take(MAX_VERIFY_ENTRYPOINT_BYTES),
        &mut destination,
    )?;
    if pinned_exe_file.read(&mut [0])? != 0 {
        return Err(std::io::Error::other("verify entrypoint exceeds 64 MiB"));
    }
    destination.sync_all()?;
    drop(destination);
    Ok(entrypoint)
}

#[allow(clippy::too_many_arguments)]
async fn run_verify_pr(
    server: &MemoryServer,
    config: &Arc<GatewayConfig>,
    gateway_actor: &A2aTransitionActor,
    dest: &DestinationPolicy,
    job: &AdmittedJobSnapshot,
    run_root: &Path,
    worktree: &Path,
    runtime_dir: &Path,
    deadline: tokio::time::Instant,
    cancel_rx: &mut watch::Receiver<bool>,
) -> TerminalVerdict {
    let Some(verify_cmd) = dest.verify_command.as_deref() else {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("verify_command_unconfigured"),
            external_ref: None,
            reaped: true,
        };
    };
    let Some(pr) = job.pull_request else {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("invalid_verify_pr_request"),
            external_ref: None,
            reaped: true,
        };
    };
    let (pinned_exe_file, current_id) = match open_verified_command_file(verify_cmd) {
        Ok(pair) => pair,
        Err(_) => {
            return TerminalVerdict {
                outcome: RemoteTerminalOutcome::Failed,
                summary_digest: None,
                error_class: Some("verify_command_changed"),
                external_ref: None,
                reaped: true,
            };
        }
    };
    if current_id != dest.verify_command_identity {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("verify_command_changed"),
            external_ref: None,
            reaped: true,
        };
    }

    let Ok(entrypoint) = stage_verify_entrypoint(pinned_exe_file, run_root) else {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("verify_entrypoint_unavailable"),
            external_ref: None,
            reaped: true,
        };
    };

    let args: Vec<OsString> = verify_cmd[1..].iter().map(OsString::from).collect();
    let mut cmd = match build_sandboxed_worker_command(
        config,
        entrypoint.as_os_str(),
        &args,
        worktree,
        runtime_dir,
        entrypoint.parent(),
    ) {
        Ok(cmd) => cmd,
        Err(err_class) => {
            return TerminalVerdict {
                outcome: RemoteTerminalOutcome::Failed,
                summary_digest: None,
                error_class: Some(err_class),
                external_ref: None,
                reaped: true,
            };
        }
    };
    cmd.env("TACHI_REMOTE_JOB", "verify_pr")
        .env("TACHI_REMOTE_REPOSITORY", &job.repository)
        .env("TACHI_REMOTE_PULL_REQUEST", pr.to_string())
        .env("TACHI_REMOTE_HEAD_SHA", &job.head_sha)
        .env("TACHI_REMOTE_LOCAL_RUN_ID", &job.local_run_id)
        .env("TACHI_REMOTE_LOCAL_CLAIM_ID", &job.local_claim_id)
        .env("TACHI_REMOTE_VERIFY_POLICY", &job.policy_profile)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    #[cfg(unix)]
    {
        cmd.process_group(0);
        unsafe {
            cmd.pre_exec(tachi_dispatch::mark_native_inherited_fds_cloexec);
        }
    }

    if let Err(error_class) = authorize_worker_start(server, gateway_actor, &job.envelope_id).await
    {
        let canceled = error_class == "canceled_before_spawn" || *cancel_rx.borrow();
        return TerminalVerdict {
            outcome: if canceled {
                RemoteTerminalOutcome::Canceled
            } else {
                RemoteTerminalOutcome::Failed
            },
            summary_digest: None,
            error_class: (!canceled).then_some(error_class),
            external_ref: None,
            reaped: true,
        };
    }

    let Ok(boot_id) = current_boot_id().and_then(|boot_id| {
        mark_spawning(config, &job.local_run_id, run_root, &boot_id)?;
        Ok(boot_id)
    }) else {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("spawn_failed"),
            external_ref: None,
            reaped: true,
        };
    };
    if *cancel_rx.borrow() {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Canceled,
            summary_digest: None,
            error_class: None,
            external_ref: None,
            reaped: true,
        };
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => {
            return TerminalVerdict {
                outcome: RemoteTerminalOutcome::Failed,
                summary_digest: None,
                error_class: Some("spawn_failed"),
                external_ref: None,
                reaped: true,
            };
        }
    };
    let pgid = child.id().map(|pid| pid as ProcessGroupId);
    let started = match mark_running(config, &job.local_run_id, run_root, child.id(), &boot_id) {
        Ok(()) => record_spawned_worker_start(server, gateway_actor, &job.envelope_id, None).await,
        Err(error_class) => Err(error_class),
    };
    if let Err(error_class) = started {
        let reaped = terminate_and_reap(&mut child, pgid).await;
        let canceled = *cancel_rx.borrow();
        return TerminalVerdict {
            outcome: if canceled {
                RemoteTerminalOutcome::Canceled
            } else {
                RemoteTerminalOutcome::Failed
            },
            summary_digest: None,
            error_class: (!canceled).then_some(error_class),
            external_ref: None,
            reaped,
        };
    }

    let mut stdout_reader = child.stdout.take();
    let mut read_stdout = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(reader) = stdout_reader.take() {
            let _ = reader
                .take((MAX_ADAPTER_OUTPUT_BYTES + 1) as u64)
                .read_to_end(&mut buf)
                .await;
        }
        buf
    });

    let exit_observed = tokio::select! {
        result = wait_for_run_leader(&mut child, pgid, deadline) => result,
        changed = cancel_rx.changed() => {
            let reaped = terminate_and_reap(&mut child, pgid).await;
            read_stdout.abort();
            if changed.is_ok() && *cancel_rx.borrow() {
                return TerminalVerdict {
                    outcome: RemoteTerminalOutcome::Canceled,
                    summary_digest: None,
                    error_class: None,
                    external_ref: None,
                    reaped,
                };
            }
            return TerminalVerdict {
                outcome: RemoteTerminalOutcome::Failed,
                summary_digest: None,
                error_class: Some("execution_interrupted"),
                external_ref: None,
                reaped,
            };
        }
    };

    let reaped = terminate_and_reap(&mut child, pgid).await;
    if matches!(exit_observed, Ok(false)) {
        read_stdout.abort();
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("execution_timeout"),
            external_ref: None,
            reaped,
        };
    }
    let exit_status = if matches!(exit_observed, Ok(true)) && reaped {
        child.wait().await.ok()
    } else {
        None
    };
    let stdout = match tokio::time::timeout(
        Duration::from_secs(STDOUT_DRAIN_TIMEOUT_SECS),
        &mut read_stdout,
    )
    .await
    {
        Ok(Ok(buf)) => buf,
        _ => {
            read_stdout.abort();
            return TerminalVerdict {
                outcome: RemoteTerminalOutcome::Failed,
                summary_digest: None,
                error_class: Some("adapter_io_error"),
                external_ref: None,
                reaped,
            };
        }
    };

    if stdout.len() > MAX_ADAPTER_OUTPUT_BYTES {
        tracing::warn!(target: LOG_TARGET, "verify_pr stdout exceeded 64 KiB bound");
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("invalid_external_receipt"),
            external_ref: None,
            reaped,
        };
    }
    let Some(status) = exit_status else {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("verify_command_failed"),
            external_ref: None,
            reaped,
        };
    };
    if !status.success() {
        return TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some("verify_command_failed"),
            external_ref: None,
            reaped,
        };
    }

    match validate_verify_pr_receipt(&stdout, &job.repository, pr, &job.head_sha) {
        Ok((RemoteTerminalOutcome::Completed, receipt_url, digest)) => TerminalVerdict {
            outcome: RemoteTerminalOutcome::Completed,
            summary_digest: Some(digest),
            error_class: None,
            external_ref: Some(receipt_url),
            reaped,
        },
        Ok((_, receipt_url, digest)) => TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: Some(digest),
            error_class: Some("verify_command_failed"),
            external_ref: Some(receipt_url),
            reaped,
        },
        Err(err_class) => TerminalVerdict {
            outcome: RemoteTerminalOutcome::Failed,
            summary_digest: None,
            error_class: Some(err_class),
            external_ref: None,
            reaped,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_drains_a_run_registered_by_an_in_flight_admission() {
        let registry = RemoteRunRegistry::new();
        let guard = registry.begin_admission().await.unwrap();
        let shutdown = registry.shutdown();
        tokio::pin!(shutdown);
        tokio::select! {
            biased;
            () = &mut shutdown => panic!("shutdown finished while admission was in flight"),
            () = std::future::ready(()) => {}
        }
        assert!(registry.begin_admission().await.is_none());

        let mut cancel = registry.register("in-flight");
        let (drained, mut observed) = tokio::sync::oneshot::channel();
        registry.track(tokio::spawn(async move {
            cancel.changed().await.unwrap();
            drained.send(*cancel.borrow()).unwrap();
        }));
        drop(guard);

        shutdown.await;
        assert!(observed.try_recv().unwrap());
    }

    #[test]
    fn recovery_preserves_unmarked_admission_and_cleans_marked_runs_at_their_original_root() {
        use memcore::db::a2a::{
            admit_remote_task, NewRemoteTask, RemoteAdapterKind, RemoteAdmissionDecision,
            RemoteJobGrant, RemoteLocalAdmissionSpec, RemoteTaskAccess, RemoteTaskHeader,
            RemoteTaskInsertOutcome,
        };
        use memcore::VerifiedAdmissionBinding;
        use rusqlite::params;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("gateway");
        create_private_dir(&home).unwrap();
        let server = MemoryServer::new_without_background_workers(
            home.join("global.db"),
            None,
            memcore::MigrationAuthority::Allow {
                approved_by: "test".into(),
            },
            home.clone(),
        )
        .unwrap();
        crate::claims_ops::admit_agent_connection(&server, Some("gateway".into()), true).unwrap();
        let (identity, connection_id, _) = server.work_claim_connection().unwrap();
        let actor = A2aTransitionActor {
            agent_identity_id: identity.unwrap(),
            connection_id,
        };
        let access = RemoteTaskAccess {
            destination_agent_identity_id: "gateway".into(),
            destination_host_id: "destination".into(),
            grants: vec![RemoteJobGrant {
                repository: "github.com/example/tachi".into(),
                job: "review_revision".into(),
            }],
        };
        let spec = RemoteLocalAdmissionSpec {
            canonical_project: "d".repeat(64),
            adapter_kind: RemoteAdapterKind::NativeReview,
            policy_profile: "codex_sol_review".into(),
            resource_slot: "review".into(),
            max_concurrent_runs: 2,
            lease_ttl_secs: 600,
            issue_ref: "example/tachi#review_revision".into(),
            worktree_path: "/fixture/repo".into(),
            writable: false,
        };
        let run_ids = server.with_global_store(|store| {
            let conn = store.connection();
            let now = memcore::now_utc_iso();
            let binding = VerifiedAdmissionBinding {
                admission_id: "origin-admission".into(),
                agent_identity_id: "origin".into(),
                connection_id: "origin-connection".into(),
                issuer_id: "gateway-issuer".into(),
                verification_method: memcore::VERIFIED_ADMISSION_MTLS_METHOD.into(),
                verification_version: "v1".into(),
                trust_domain: "workspace:test".into(),
                verification_scope: "agent_identity:remote_admission".into(),
            };
            conn.execute("INSERT INTO agent_identities(agent_identity_id,created_at) VALUES ('origin',?1)", [&now]).unwrap();
            conn.execute(
                "INSERT INTO identity_admissions(admission_id,agent_identity_id,connection_id,state,created_at)
                 VALUES ('origin-admission','origin','origin-connection','verified',?1)", [&now],
            ).unwrap();
            conn.execute(
                "INSERT INTO identity_admission_verification_receipts
                 (receipt_id,admission_id,agent_identity_id,connection_id,issuer_id,verification_method,
                  verification_version,trust_domain,verification_scope,evidence_digest,evidence_ref,
                  evidence_issued_at,evidence_expires_at,nonce,idempotency_key,request_digest,current_state,verified_at)
                 VALUES ('origin-receipt','origin-admission','origin','origin-connection','gateway-issuer',?1,
                         'v1','workspace:test','agent_identity:remote_admission',?2,?3,
                         strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'),
                         strftime('%Y-%m-%dT%H:%M:%fZ','now','+1 day'),'origin-nonce','origin-key',?2,'verified',?4)",
                params![binding.verification_method, "a".repeat(64), format!("attestation:mtls-peer:{}", "b".repeat(64)), now],
            ).unwrap();
            let expires_at: String = conn.query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','+5 minutes')", [], |row| row.get(0),
            ).unwrap();
            Ok(["admit-only", "executed"].map(|id| {
                let repository = access.grants[0].repository.clone();
                let head_sha = "a".repeat(40);
                let body = json!({
                    "kind": "tachi.remote_job/v1", "sender": "origin", "recipient": "gateway",
                    "trust_domain": binding.trust_domain, "idempotency_key": id, "nonce": id,
                    "issued_at": now, "expires_at": expires_at, "repository": repository,
                    "head_sha": head_sha, "origin_claim": {"claim_id": "origin-claim", "transition_version": 1},
                    "job": "review_revision", "question": "Review this fixture."
                });
                let task = admit_remote_task(
                    conn, &binding, &actor, &access,
                    &NewRemoteTask {
                        envelope_id: id.into(),
                        canonical_body: body.to_string(),
                        nonce: id.into(),
                        idempotency_key: id.into(),
                        issued_at: now.clone(),
                        expires_at: expires_at.clone(),
                        recipient_agent_identity_id: "gateway".into(),
                        header: RemoteTaskHeader {
                            repository, head_sha, job: "review_revision".into(), pull_request: None,
                            origin_claim_id: "origin-claim".into(), origin_claim_version: 1,
                            destination_host_id: "destination".into(),
                        },
                    },
                    &RemoteAdmissionDecision::Admit(spec.clone()),
                ).unwrap();
                let RemoteTaskInsertOutcome::Created(task) = task else { panic!("expected new task") };
                task.run.unwrap().local_run_id
            }))
        }).unwrap();
        let mut config: GatewayConfig = serde_json::from_value(json!({
            "schema_version": "tachi.remote_gateway.v1", "agent_identity_id": "gateway",
            "host_id": "destination", "issuer_id": "gateway-issuer", "trust_domain": "workspace:test",
            "home": home, "bind": "127.0.0.1:0", "certificate": home.join("certificate"),
            "private_key": home.join("key"), "peer_ca": home.join("ca"), "peers": [],
        })).unwrap();
        config.canonical_home = home;
        config.canonical_worker_runs_dir = Some(root.join("original-worker-runs"));
        let original_run_root = remote_run_root(&config, &run_ids[1]);
        let auth = original_run_root.join("runtime/home/.codex/auth.json");
        create_private_dir(auth.parent().unwrap()).unwrap();
        std::fs::write(&auth, r#"{"fixture":"provider-auth"}"#).unwrap();
        create_private_dir(&original_run_root.join("worktree")).unwrap();
        write_pgid_marker(
            &config,
            &run_ids[1],
            &original_run_root,
            json!({"state": "admitted_for_execution"}),
        )
        .unwrap();

        config.canonical_worker_runs_dir = Some(root.join("restarted-worker-runs"));
        let restarted_run_root = remote_run_root(&config, &run_ids[1]);
        create_private_dir(&restarted_run_root).unwrap();
        recover_orphaned_gateway_runs(&server, &config, &actor).unwrap();

        assert!(precheck_launch_authority(&server, &actor, "admit-only").is_ok());
        assert!(!original_run_root.exists());
        assert!(restarted_run_root.is_dir());
        assert!(!pgid_file(&config, &run_ids[1]).exists());
        server.with_global_store_read(|store| {
            let conn = store.connection();
            let state = |id| {
                conn.query_row(
                    "SELECT lifecycle_state, cleanup_state FROM a2a_remote_runs WHERE envelope_id=?1",
                    [id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                ).unwrap()
            };
            assert_eq!(state("admit-only"), ("admitted".into(), "pending".into()));
            assert_eq!(state("executed"), ("failed".into(), "cleaned".into()));
            let error_class: String = conn.query_row(
                "SELECT json_extract(detail, '$.error_class') FROM a2a_delivery_receipts
                 WHERE envelope_id='executed' AND state='remote_failed'", [], |row| row.get(0),
            ).unwrap();
            assert_eq!(error_class, "gateway_restarted_run_orphaned");
            Ok(())
        }).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_leader_exit_is_observed_without_releasing_the_pid() {
        let mut child = Command::new("/usr/bin/true")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let pgid = Some(pid as libc::pid_t);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        assert!(wait_for_run_leader(&mut child, pgid, deadline)
            .await
            .unwrap());
        assert_eq!(child.id(), Some(pid));
        assert!(terminate_and_reap(&mut child, pgid).await);
        assert!(child.wait().await.unwrap().success());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn recovery_leaves_a_live_group_with_a_different_boot_or_start_token_alone() {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pgid = child.id().unwrap() as libc::pid_t;
        let boot = "current-boot";
        let token = process_start_token(pgid).unwrap();
        assert!(recover_process_group(
            &RunPgidMarker {
                state: "running".into(),
                run_root: None,
                pgid: Some(pgid),
                boot_id: Some(format!("{boot}-old")),
                leader_start_token: Some(token.clone()),
            },
            boot
        )
        .is_ok());
        assert!(recover_process_group(
            &RunPgidMarker {
                state: "running".into(),
                run_root: None,
                pgid: Some(pgid),
                boot_id: Some(boot.to_owned()),
                leader_start_token: Some(format!("{token}-old")),
            },
            boot
        )
        .is_err());
        assert!(child.try_wait().unwrap().is_none());
        assert!(terminate_and_reap(&mut child, Some(pgid)).await);
    }

    #[cfg(unix)]
    #[test]
    fn verify_entrypoint_uses_the_open_inode_after_the_source_path_is_replaced() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("adapter");
        std::fs::write(&source, b"original").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (file, _) = open_verified_command_file(&[source.to_str().unwrap().into()]).unwrap();

        let replacement = root.path().join("replacement");
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::rename(&replacement, &source).unwrap();

        let entrypoint = stage_verify_entrypoint(file, root.path()).unwrap();
        assert_eq!(std::fs::read(&entrypoint).unwrap(), b"original");
        assert_eq!(
            entrypoint.metadata().unwrap().permissions().mode() & 0o777,
            0o500
        );
    }

    #[test]
    fn verify_pr_receipt_rejects_explicit_github_ports() {
        let sha = "a".repeat(40);
        for port in [8443, 443] {
            let receipt = serde_json::to_vec(&json!({
                "status": "completed",
                "repository": "owner/repo",
                "pull_request": 1,
                "head_sha": sha,
                "receipt_url": format!("https://github.com:{port}/owner/repo/pull/1#issuecomment-1")
            }))
            .unwrap();
            assert_eq!(
                validate_verify_pr_receipt(&receipt, "owner/repo", 1, &sha),
                Err("invalid_external_receipt")
            );
        }
    }

    #[test]
    fn verify_pr_receipt_requires_exact_repo_head_and_github_receipt_url() {
        let sha = "a".repeat(40);
        let valid = serde_json::to_vec(&json!({
            "status": "completed",
            "repository": "owner/repo",
            "pull_request": 42,
            "head_sha": sha,
            "receipt_url": "https://github.com/owner/repo/pull/42#issuecomment-12345"
        }))
        .unwrap();
        let (outcome, url, digest) =
            validate_verify_pr_receipt(&valid, "owner/repo", 42, &sha).unwrap();
        assert_eq!(outcome, RemoteTerminalOutcome::Completed);
        assert_eq!(
            url,
            "https://github.com/owner/repo/pull/42#issuecomment-12345"
        );
        assert_eq!(digest.len(), 64);

        let valid_check = serde_json::to_vec(&json!({
            "status": "completed",
            "repository": "owner/repo",
            "pull_request": 42,
            "head_sha": sha,
            "receipt_url": "https://github.com/owner/repo/pull/42/checks?check_run_id=987654"
        }))
        .unwrap();
        assert!(validate_verify_pr_receipt(&valid_check, "owner/repo", 42, &sha).is_ok());

        // Missing repository field must be rejected.
        let missing_repo = serde_json::to_vec(&json!({
            "status": "completed",
            "pull_request": 42,
            "head_sha": sha,
            "receipt_url": "https://github.com/owner/repo/pull/42#issuecomment-12345"
        }))
        .unwrap();
        assert_eq!(
            validate_verify_pr_receipt(&missing_repo, "owner/repo", 42, &sha),
            Err("invalid_external_receipt")
        );

        // PR /files subpage without receipt comment/check_run_id must be rejected.
        let files_page = serde_json::to_vec(&json!({
            "status": "completed",
            "repository": "owner/repo",
            "pull_request": 42,
            "head_sha": sha,
            "receipt_url": "https://github.com/owner/repo/pull/42/files"
        }))
        .unwrap();
        assert_eq!(
            validate_verify_pr_receipt(&files_page, "owner/repo", 42, &sha),
            Err("invalid_external_receipt")
        );

        // Wrong head_sha must be rejected.
        let wrong_sha = serde_json::to_vec(&json!({
            "status": "completed",
            "repository": "owner/repo",
            "pull_request": 42,
            "head_sha": "b".repeat(40),
            "receipt_url": "https://github.com/owner/repo/pull/42#issuecomment-12345"
        }))
        .unwrap();
        assert_eq!(
            validate_verify_pr_receipt(&wrong_sha, "owner/repo", 42, &sha),
            Err("invalid_external_receipt")
        );

        // Plain prose exit-0 stdout is never promoted to completed.
        assert_eq!(
            validate_verify_pr_receipt(b"all tests passed!", "owner/repo", 42, &sha),
            Err("invalid_external_receipt")
        );
    }
}
