use super::dispatch::DispatchResult;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;

const DEFAULT_OPENCODE_SOP_MAX_CONCURRENCY: usize = 2;
const MAX_OPENCODE_SOP_CONCURRENCY: usize = 32;
const PROCESS_GROUP_TERM_GRACE: Duration = Duration::from_millis(1500);

static OPENCODE_SOP_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

pub(super) async fn run_agent_subprocess(
    mut cmd: Command,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    run_agent_subprocess_inner(&mut cmd, timeout, None).await
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

        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let run = tokio::spawn(run_agent_subprocess(cmd, Duration::from_secs(1)));
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
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
}
