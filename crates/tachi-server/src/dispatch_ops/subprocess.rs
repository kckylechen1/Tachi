use super::dispatch::DispatchResult;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

const DEFAULT_OPENCODE_SOP_MAX_CONCURRENCY: usize = 2;
const MAX_OPENCODE_SOP_CONCURRENCY: usize = 32;
const PROCESS_GROUP_TERM_GRACE: Duration = Duration::from_millis(1500);

static OPENCODE_SOP_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

#[cfg(test)]
static MANAGED_CANCEL_PROBE_FAILURE_RUN_DIR: OnceLock<
    std::sync::Mutex<Option<std::path::PathBuf>>,
> = OnceLock::new();

pub(super) async fn run_agent_subprocess(
    mut cmd: Command,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    run_agent_subprocess_inner(&mut cmd, timeout, None).await
}

pub(super) async fn run_managed_custom_subprocess(
    mut cmd: Command,
    timeout: Duration,
    mut cancellations: mpsc::Receiver<crate::managed_run_control::ManagedCancelCommand>,
    run_dir: &std::path::Path,
) -> Result<DispatchResult, String> {
    #[cfg(not(unix))]
    {
        let _ = cancellations;
        let _ = run_dir;
        return Err("cancellation_unavailable: unsupported_platform".to_string());
    }
    #[cfg(unix)]
    {
        if let Ok(command) = cancellations.try_recv() {
            return finish_pre_spawn_cancellation(command, run_dir);
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        configure_process_group(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn agent process: {e}"))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_task = tokio::spawn(read_pipe(stdout));
        let stderr_task = tokio::spawn(read_pipe(stderr));
        let status = tokio::select! {
            result = tokio::time::timeout(timeout, child.wait()) => match result {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => {
                    drain_managed_output(stdout_task, stderr_task).await;
                    return Err(format!("Agent process error: {error}"));
                }
                Err(_) => {
                    reap_timed_out_child(&mut child, pid).await;
                    drain_managed_output(stdout_task, stderr_task).await;
                    return Err(format!("Agent process timed out after {}s (process group killed)", timeout.as_secs()));
                }
            },
            command = cancellations.recv() => {
                let Some(command) = command else {
                    reap_timed_out_child(&mut child, pid).await;
                    drain_managed_output(stdout_task, stderr_task).await;
                    return Err("managed cancellation channel closed".to_string());
                };
                let status = match try_wait_for_managed_cancellation(&mut child, run_dir) {
                    Ok(status) => status,
                    Err(error) => {
                        reap_timed_out_child(&mut child, pid).await;
                        drain_managed_output(stdout_task, stderr_task).await;
                        let _ = command.response.send(
                            crate::managed_run_control::CancelCompletion::Unavailable(
                                "child_probe_failed",
                            ),
                        );
                        return Err(format!("managed cancellation child probe failed: {error}"));
                    }
                };
                if let Some(status) = status {
                    let _ = command.response.send(crate::managed_run_control::CancelCompletion::Unavailable("completion_winner"));
                    return finish_managed_output(status, stdout_task, stderr_task).await;
                }
                reap_timed_out_child(&mut child, pid).await;
                drain_managed_output(stdout_task, stderr_task).await;
                if !wait_for_process_group_absence(pid).await {
                    let _ = crate::managed_run_control::record_termination_unconfirmed(run_dir, command.expected_status_revision);
                    let _ = command.response.send(crate::managed_run_control::CancelCompletion::Unconfirmed);
                    return Err("termination_unconfirmed".to_string());
                }
                match crate::managed_run_control::confirm_managed_custom_cancellation(run_dir, command.expected_status_revision, "unix_process_group_absent") {
                    Ok(status_revision) => {
                        let _ = command.response.send(crate::managed_run_control::CancelCompletion::Confirmed {
                            termination_proof: "unix_process_group_absent",
                            status_revision,
                        });
                        return Err("managed_cancelled".to_string());
                    }
                    Err(_) => {
                        let _ = command.response.send(crate::managed_run_control::CancelCompletion::Unconfirmed);
                        return Err("termination_unconfirmed".to_string());
                    }
                }
            }
        };
        finish_managed_output(status, stdout_task, stderr_task).await
    }
}

#[cfg(unix)]
fn process_group_absent(pid: Option<u32>) -> bool {
    let Some(pid) = pid else {
        return true;
    };
    let rc = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
    rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
async fn wait_for_process_group_absence(pid: Option<u32>) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if process_group_absent(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    process_group_absent(pid)
}

#[cfg(unix)]
fn finish_pre_spawn_cancellation(
    command: crate::managed_run_control::ManagedCancelCommand,
    run_dir: &std::path::Path,
) -> Result<DispatchResult, String> {
    match crate::managed_run_control::confirm_managed_custom_cancellation(
        run_dir,
        command.expected_status_revision,
        "spawn_suppressed",
    ) {
        Ok(status_revision) => {
            let _ =
                command
                    .response
                    .send(crate::managed_run_control::CancelCompletion::Confirmed {
                        termination_proof: "spawn_suppressed",
                        status_revision,
                    });
            Err("managed_cancelled".to_string())
        }
        Err(_) => {
            let _ = command
                .response
                .send(crate::managed_run_control::CancelCompletion::Unconfirmed);
            Err("termination_unconfirmed".to_string())
        }
    }
}

async fn finish_managed_output(
    status: std::process::ExitStatus,
    stdout_task: tokio::task::JoinHandle<Vec<u8>>,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
) -> Result<DispatchResult, String> {
    let stdout = collect_pipe(stdout_task).await?;
    let stderr = collect_pipe(stderr_task).await?;
    let output = if stdout.is_empty() && !stderr.is_empty() {
        stderr
    } else if !stderr.is_empty() {
        format!("{stdout}\n\n--- stderr ---\n{stderr}")
    } else {
        stdout
    };
    Ok(DispatchResult {
        output,
        exit_code: status.code(),
        observed_model: None,
    })
}

/// Managed cancellation owns the child process group and both pipe readers.
/// After reaping the group, await the readers before publishing an outcome so a
/// confirmed cancellation cannot leave detached reader tasks behind.
async fn drain_managed_output(
    stdout_task: tokio::task::JoinHandle<Vec<u8>>,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
) {
    let _ = tokio::join!(collect_pipe(stdout_task), collect_pipe(stderr_task));
}

fn try_wait_for_managed_cancellation(
    child: &mut tokio::process::Child,
    _run_dir: &std::path::Path,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    #[cfg(test)]
    {
        let configured_run_dir = MANAGED_CANCEL_PROBE_FAILURE_RUN_DIR
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if configured_run_dir.as_deref() == Some(_run_dir) {
            return Err(std::io::Error::other(
                "injected managed cancellation probe failure",
            ));
        }
    }
    child.try_wait()
}

#[cfg(test)]
struct ManagedCancelProbeFailureGuard;

#[cfg(test)]
impl Drop for ManagedCancelProbeFailureGuard {
    fn drop(&mut self) {
        let mut configured_run_dir = MANAGED_CANCEL_PROBE_FAILURE_RUN_DIR
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *configured_run_dir = None;
    }
}

#[cfg(test)]
fn inject_managed_cancel_probe_failure(
    run_dir: &std::path::Path,
) -> ManagedCancelProbeFailureGuard {
    let mut configured_run_dir = MANAGED_CANCEL_PROBE_FAILURE_RUN_DIR
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        configured_run_dir.replace(run_dir.to_path_buf()).is_none(),
        "managed cancellation probe failure already configured"
    );
    ManagedCancelProbeFailureGuard
}

pub(super) async fn run_opencode_sop_subprocess(
    mut cmd: Command,
    timeout: Duration,
    sop_label: &str,
) -> Result<DispatchResult, String> {
    let _permit = opencode_sop_semaphore()
        .acquire()
        .await
        .map_err(|e| format!("opencode SOP subprocess limiter closed: {e}"))?;
    run_agent_subprocess_inner(&mut cmd, timeout, Some(sop_label)).await
}

async fn run_agent_subprocess_inner(
    cmd: &mut Command,
    timeout: Duration,
    opencode_sop_label: Option<&str>,
) -> Result<DispatchResult, String> {
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // If this task is dropped (daemon shutdown, spawning task cancelled, ...),
    // tokio kills the child via SIGKILL instead of leaving it orphaned.
    cmd.kill_on_drop(true);
    configure_process_group(cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn agent process: {e}"))?;
    let child_pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = tokio::spawn(read_pipe(stdout));
    let stderr_task = tokio::spawn(read_pipe(stderr));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(res) => {
            let status = res.map_err(|e| format!("Agent process error: {e}"))?;
            terminate_process_group(child_pid, libc::SIGTERM);
            status
        }
        Err(_) => {
            reap_timed_out_child(&mut child, child_pid).await;
            if let Some(sop_label) = opencode_sop_label {
                tracing::warn!(
                    sop = sop_label,
                    elapsed_secs = timeout.as_secs(),
                    "daemon opencode SOP subprocess timed out; child process group killed"
                );
            }
            return Err(format!(
                "Agent process timed out after {}s (process group killed)",
                timeout.as_secs()
            ));
        }
    };

    let exit_code = status.code();
    let stdout = collect_pipe(stdout_task).await?;
    let stderr = collect_pipe(stderr_task).await?;

    let output_text = if stdout.is_empty() && !stderr.is_empty() {
        stderr
    } else if !stderr.is_empty() {
        format!("{}\n\n--- stderr ---\n{}", stdout, stderr)
    } else {
        stdout
    };

    Ok(DispatchResult {
        output: output_text,
        exit_code,
        observed_model: None,
    })
}

async fn read_pipe(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes).await;
    bytes
}

async fn collect_pipe(task: tokio::task::JoinHandle<Vec<u8>>) -> Result<String, String> {
    let bytes = task
        .await
        .map_err(|e| format!("Agent output reader task failed: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

async fn reap_timed_out_child(child: &mut tokio::process::Child, child_pid: Option<u32>) {
    terminate_process_group(child_pid, libc::SIGTERM);
    let child_exited = match tokio::time::timeout(PROCESS_GROUP_TERM_GRACE, child.wait()).await {
        Ok(Ok(_)) => true,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "failed while waiting for timed-out subprocess");
            false
        }
        Err(_) => false,
    };
    terminate_process_group(child_pid, libc::SIGKILL);
    if !child_exited {
        if let Err(err) = child.kill().await {
            tracing::warn!(error = %err, "failed to kill timed-out subprocess");
        }
        let _ = child.wait().await;
    }
}

fn opencode_sop_semaphore() -> &'static Semaphore {
    OPENCODE_SOP_SEMAPHORE.get_or_init(|| {
        Semaphore::new(env_usize(
            "TACHI_OPENCODE_SOP_MAX_CONCURRENCY",
            DEFAULT_OPENCODE_SOP_MAX_CONCURRENCY,
            MAX_OPENCODE_SOP_CONCURRENCY,
        ))
    })
}

fn env_usize(name: &str, default: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(1, max)
}

#[cfg(unix)]
fn configure_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
fn terminate_process_group(child_pid: Option<u32>, signal: libc::c_int) {
    let Some(pid) = child_pid else {
        return;
    };
    let pgid = -(pid as libc::pid_t);
    // SAFETY: `kill(pgid, signal)` sends a signal to an OS process group; it
    // passes no pointers across the FFI boundary and aliases no Rust memory.
    // `pgid` is a negative pid_t derived from the child pid; an invalid group
    // yields ESRCH and is tolerated below.
    let rc = unsafe { libc::kill(pgid, signal) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(pid, signal, error = %err, "process group signal failed");
        }
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_child_pid: Option<u32>, _signal: libc::c_int) {}

pub(super) fn tail_chars(text: &str, max_chars: usize) -> String {
    tachi_dispatch::tail_chars(text, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn run_agent_subprocess_closes_child_stdin() {
        let cmd = Command::new("/bin/cat");

        let result = run_agent_subprocess(cmd, Duration::from_secs(5))
            .await
            .expect("stdin reader should observe EOF and exit");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.output, "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_agent_subprocess_timeout_reaps_child_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script_path = temp.path().join("hung-opencode");
        let child_pid_path = temp.path().join("child.pid");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nsh -c 'trap \"\" TERM; sleep 60' &\necho $! > '{}'\nwait\n",
                child_pid_path.display()
            ),
        )
        .expect("write fake opencode");
        let mut perms = std::fs::metadata(&script_path)
            .expect("metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod");
        let mut cmd = Command::new(&script_path);
        let child_pid_path_for_wait = child_pid_path.clone();
        cmd.env(
            "PATH",
            std::env::var_os("PATH").expect("PATH must exist for shell fixture"),
        );

        // Root cause (issue #997, confirmed via tracing): the simulated
        // "agent hung" timeout passed to `run_agent_subprocess` shared the
        // same budget as the fixture's own startup latency (fork+exec `sh`,
        // background a sub-shell, `echo $! > file`). With a 1s simulated
        // timeout, `run_agent_subprocess_inner`'s internal `tokio::time::timeout`
        // occasionally fired and SIGTERM'd the fixture's process group BEFORE
        // the fixture reached its `echo $! > file` line under scheduler
        // latency (observed even with zero build contention, an isolated
        // target dir, and a quiet machine) — once the process group is
        // killed, the pid file can never be written, so no `wait_for_file`
        // deadline, however large, fixes this: the resource being polled for
        // genuinely never gets created. Widening the simulated timeout to 4s
        // gives the fixture's startup line real headroom to run before the
        // timeout fires, decoupling "time for the fixture to announce itself"
        // from "time until the simulated hang is treated as timed out" —
        // this still exercises a real reap (the fixture's `sleep 60` still
        // vastly outlasts 4s) without weakening what the test proves.
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let run = tokio::spawn(run_agent_subprocess(cmd, Duration::from_secs(4)));
            wait_for_file(&child_pid_path_for_wait).await;
            run.await.expect("subprocess runner task should not panic")
        })
        .await
        .expect("runner must return instead of hanging forever");

        let Err(error) = result else {
            panic!("hung fake opencode should time out");
        };
        assert!(
            error.contains("timed out"),
            "timeout should surface as subprocess error: {error}"
        );
        let child_pid: libc::pid_t = std::fs::read_to_string(&child_pid_path)
            .expect("child pid")
            .trim()
            .parse()
            .expect("pid integer");
        assert!(
            wait_for_process_exit(child_pid).await,
            "hung child process {child_pid} should be reaped"
        );
    }

    #[cfg(unix)]
    async fn wait_for_file(path: &std::path::Path) {
        // 10s: a poll-with-deadline safety net on fork+exec/scheduler latency
        // for a trivial shell fixture (not a fixed sleep, and not a
        // functional wait — it normally resolves in well under a second).
        // The real fix for issue #997's "fixture did not write child pid
        // file" flake is the widened simulated-timeout in the caller above
        // (this deadline being too short was never the root cause: tracing
        // showed the fixture's process group was killed by the *simulated*
        // 1s timeout before it could reach `echo $! > file`, so the file
        // could never appear no matter how long this polled).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fixture did not write child pid file: {}", path.display());
    }

    #[cfg(unix)]
    async fn wait_for_process_exit(pid: libc::pid_t) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            // SAFETY: `kill(pid, 0)` performs a signal-0 existence probe — it
            // sends no signal, passes no pointers across the FFI boundary, and
            // aliases no Rust memory.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staff_cancel_managed_custom_process_confirms_group_termination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("managed-custom");
        let descendant = temp.path().join("descendant.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntrap '' TERM\nsh -c 'trap \"\" TERM; sleep 60' &\necho $! > '{}'\nwait\n",
                descendant.display()
            ),
        )
        .expect("write fixture");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "20260823T010100Z-custom-deadbeef",
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
                "cancellation": {"receipt": "cancellation_requested"},
            })
            .to_string(),
        )
        .expect("status");

        let (sender, receiver) = mpsc::channel(1);
        let (reply, confirmed) = tokio::sync::oneshot::channel();
        let command = Command::new(&script);
        let run_dir_for_task = run_dir.clone();
        let run = tokio::spawn(async move {
            run_managed_custom_subprocess(
                command,
                Duration::from_secs(20),
                receiver,
                &run_dir_for_task,
            )
            .await
        });
        wait_for_file(&descendant).await;
        sender
            .send(crate::managed_run_control::ManagedCancelCommand {
                expected_status_revision: 1,
                response: reply,
            })
            .await
            .expect("private cancellation channel is open");
        let result = run.await.expect("runner task does not panic");
        match result {
            Err(error) => assert_eq!(error, "managed_cancelled"),
            Ok(_) => panic!("cancellation must interrupt the managed root"),
        }
        assert!(matches!(
            confirmed.await.expect("cancellation outcome"),
            crate::managed_run_control::CancelCompletion::Confirmed {
                termination_proof: "unix_process_group_absent",
                ..
            }
        ));
        let pid: libc::pid_t = std::fs::read_to_string(&descendant)
            .expect("descendant pid")
            .trim()
            .parse()
            .expect("numeric descendant pid");
        assert!(
            wait_for_process_exit(pid).await,
            "descendant must be absent before cancellation confirmation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_custom_probe_failure_reaps_group_and_reports_unavailable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("managed-custom");
        let root = temp.path().join("root.pid");
        let descendant = temp.path().join("descendant.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntrap '' TERM\nprintf '%s\\n' \"$$\" > '{}'\nsh -c 'trap \"\" TERM; sleep 60' &\nprintf '%s\\n' \"$!\" > '{}'\nwait\n",
                root.display(),
                descendant.display()
            ),
        )
        .expect("write fixture");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "20260823T010102Z-custom-deadbeef",
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
                "cancellation": {"receipt": "cancellation_requested"},
            })
            .to_string(),
        )
        .expect("status");

        let _probe_failure = inject_managed_cancel_probe_failure(&run_dir);
        let (sender, receiver) = mpsc::channel(1);
        let (reply, outcome) = tokio::sync::oneshot::channel();
        let command = Command::new(&script);
        let run_dir_for_task = run_dir.clone();
        let run = tokio::spawn(async move {
            run_managed_custom_subprocess(
                command,
                Duration::from_secs(20),
                receiver,
                &run_dir_for_task,
            )
            .await
        });
        wait_for_file(&root).await;
        wait_for_file(&descendant).await;
        sender
            .send(crate::managed_run_control::ManagedCancelCommand {
                expected_status_revision: 1,
                response: reply,
            })
            .await
            .expect("private cancellation channel is open");

        let result = run.await.expect("runner task does not panic");
        let descendant_pid: libc::pid_t = std::fs::read_to_string(&descendant)
            .expect("descendant pid")
            .trim()
            .parse()
            .expect("numeric descendant pid");
        let descendant_reaped = wait_for_process_exit(descendant_pid).await;
        if !descendant_reaped {
            let root_pid: u32 = std::fs::read_to_string(&root)
                .expect("root pid")
                .trim()
                .parse()
                .expect("numeric root pid");
            terminate_process_group(Some(root_pid), libc::SIGKILL);
        }
        assert!(
            descendant_reaped,
            "probe failure must still reap the managed descendant process group"
        );
        assert!(matches!(
            result,
            Err(ref error) if error.contains("managed cancellation child probe failed")
        ));
        assert!(matches!(
            outcome.await.expect("explicit cancellation outcome"),
            crate::managed_run_control::CancelCompletion::Unavailable("child_probe_failed")
        ));
    }
}

#[cfg(test)]
mod issue_1825_cancel_tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_custom_cancel_does_not_kill_unrelated_process() {
        let mut unrelated = Command::new("/bin/sh");
        unrelated.arg("-c").arg("sleep 30");
        let mut unrelated = unrelated.spawn().expect("start unrelated fixture");
        let unrelated_pid = unrelated.id().expect("unrelated pid");

        let temp = tempfile::tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "20260823T010101Z-custom-deadbeef",
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
                "cancellation": {"receipt": "cancellation_requested"},
            })
            .to_string(),
        )
        .expect("status");
        let (sender, receiver) = mpsc::channel(1);
        let (reply, outcome) = tokio::sync::oneshot::channel();
        sender
            .send(crate::managed_run_control::ManagedCancelCommand {
                expected_status_revision: 1,
                response: reply,
            })
            .await
            .expect("queue pre-spawn cancellation");
        let result = run_managed_custom_subprocess(
            Command::new("/bin/true"),
            Duration::from_secs(5),
            receiver,
            &run_dir,
        )
        .await;
        match result {
            Err(error) => assert_eq!(error, "managed_cancelled"),
            Ok(_) => panic!("pre-spawn cancellation interrupts run"),
        }
        assert!(matches!(
            outcome.await.expect("outcome"),
            crate::managed_run_control::CancelCompletion::Confirmed {
                termination_proof: "spawn_suppressed",
                ..
            }
        ));
        let alive = unsafe { libc::kill(unrelated_pid as libc::pid_t, 0) } == 0;
        let _ = unrelated.kill().await;
        let _ = unrelated.wait().await;
        assert!(
            alive,
            "managed cancellation must not target an unrelated PID"
        );
    }
}
