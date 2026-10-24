//! The one `gh` subprocess executor (audit E1).
//!
//! Every in-daemon `gh` invocation goes through [`GhCall`]. One deadline
//! covers the whole call: preparation (in-process `gh` PATH lookup and vault
//! credential resolution, run on the blocking pool so it cannot stall the
//! async executor) and execution (spawn, stdout/stderr collection, exit).
//!
//! Preparation spawns no process, so a deadline that fires while it is still
//! running abandons only in-process work and cannot leave a command behind.
//! The `gh` child is spawned only after preparation returns inside the
//! deadline. When the deadline fires during execution, the child is killed
//! explicitly and then awaited (reaped) for a short grace period;
//! `kill_on_drop` remains as a backstop and does not replace the explicit
//! kill.
//!
//! Only reads run here; nothing in the daemon changes GitHub state through
//! `gh`. The executor never retries.

use super::{build_gh_command, sanitize_output, Duration, MemoryServer, MAX_GH_OUTPUT_CHARS};
use std::ffi::{OsStr, OsString};
use std::process::{ExitStatus, Stdio};
use tokio::io::AsyncReadExt;

/// Default bound for a single-object `gh` read (`issue view`, `pr view`).
pub(crate) const GH_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for the kernel to report a killed child's exit.
const GH_REAP_GRACE: Duration = Duration::from_secs(5);
/// Stderr kept in an exit-failure diagnostic (same limit as the old runner).
const GH_STDERR_DIAGNOSTIC_CHARS: usize = 1000;

/// Where the deadline fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GhTimeoutStage {
    /// Before any `gh` process was spawned. Nothing reached GitHub.
    Prepare,
    /// While the spawned `gh` process was running. It has been killed.
    Execute,
}

/// Typed failure of a bounded `gh` call.
#[derive(Debug)]
pub(crate) enum GhRunError {
    /// Resolving the `gh` binary or credential failed. Nothing was spawned.
    Prepare(String),
    /// The `gh` process could not be spawned or awaited.
    Execute(String),
    /// `gh` ran and exited non-zero. `stderr` is sanitized and truncated.
    Exit { code: i32, stderr: String },
    /// The deadline elapsed.
    TimedOut {
        context: String,
        timeout: Duration,
        stage: GhTimeoutStage,
    },
}

impl std::fmt::Display for GhRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prepare(reason) => f.write_str(reason),
            Self::Execute(reason) => write!(f, "Failed to execute `gh`: {reason}"),
            Self::Exit { code, stderr } => write!(f, "gh failed (exit {code}): {stderr}"),
            Self::TimedOut {
                context,
                timeout,
                stage,
            } => match stage {
                GhTimeoutStage::Prepare => write!(
                    f,
                    "{context} timed out after {timeout:?} while preparing the gh command; \
                     no gh process was started"
                ),
                GhTimeoutStage::Execute => write!(
                    f,
                    "{context} timed out after {timeout:?}; the gh process was killed"
                ),
            },
        }
    }
}

impl From<GhRunError> for String {
    fn from(error: GhRunError) -> Self {
        error.to_string()
    }
}

/// Captured output of a finished `gh` process. The credential used for the
/// call is kept only to sanitize the streams and is never printed.
pub(crate) struct GhOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    token: String,
}

impl std::fmt::Debug for GhOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhOutput")
            .field("status", &self.status)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish_non_exhaustive()
    }
}

impl GhOutput {
    pub(crate) fn success(&self) -> bool {
        self.status.success()
    }

    pub(crate) fn sanitized_stdout(&self) -> String {
        sanitize_output(&String::from_utf8_lossy(&self.stdout), &self.token)
    }

    pub(crate) fn sanitized_stderr(&self) -> String {
        sanitize_output(&String::from_utf8_lossy(&self.stderr), &self.token)
    }

    /// Raw parts for callers that must inspect unsanitized bytes internally
    /// (see `bounded_json::decode_output`).
    pub(crate) fn into_parts(self) -> (std::process::Output, String) {
        (
            std::process::Output {
                status: self.status,
                stdout: self.stdout,
                stderr: self.stderr,
            },
            self.token,
        )
    }

    fn exit_error(&self) -> GhRunError {
        GhRunError::Exit {
            code: self.status.code().unwrap_or(-1),
            stderr: self
                .sanitized_stderr()
                .chars()
                .take(GH_STDERR_DIAGNOSTIC_CHARS)
                .collect(),
        }
    }
}

/// A `gh` invocation description: arguments plus how it may run. Built like
/// a `Command`, then executed once under a single deadline.
pub(crate) struct GhCall {
    timeout: Duration,
    context: Option<String>,
    args: Vec<OsString>,
    #[cfg(all(test, unix))]
    spawn_receipt: Option<SpawnReceipt>,
    #[cfg(test)]
    prepare_receipt: Option<tokio::sync::oneshot::Sender<()>>,
}

#[cfg(all(test, unix))]
enum SpawnReceipt {
    Async(tokio::sync::oneshot::Sender<u32>),
    Blocking(std::sync::mpsc::Sender<u32>),
}

#[cfg(all(test, unix))]
impl SpawnReceipt {
    fn send(self, pid: u32) {
        match self {
            Self::Async(receipt) => {
                let _ = receipt.send(pid);
            }
            Self::Blocking(receipt) => {
                let _ = receipt.send(pid);
            }
        }
    }
}

impl GhCall {
    /// A read that cannot change remote state, bounded by [`GH_READ_TIMEOUT`].
    pub(crate) fn read() -> Self {
        Self {
            timeout: GH_READ_TIMEOUT,
            context: None,
            args: Vec::new(),
            #[cfg(all(test, unix))]
            spawn_receipt: None,
            #[cfg(test)]
            prepare_receipt: None,
        }
    }

    pub(crate) fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    pub(crate) fn timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = timeout;
        self
    }

    /// Label used in timeout diagnostics. Defaults to `gh <arg0> <arg1>`.
    pub(crate) fn context(&mut self, context: impl Into<String>) -> &mut Self {
        self.context = Some(context.into());
        self
    }

    fn default_context(&self) -> String {
        let mut context = String::from("gh");
        for arg in self.args.iter().take(2) {
            context.push(' ');
            context.push_str(&arg.to_string_lossy());
        }
        context
    }

    /// Run to completion under the deadline and return the raw output,
    /// whatever the exit status.
    pub(crate) async fn output(self, server: &MemoryServer) -> Result<GhOutput, GhRunError> {
        #[cfg(test)]
        super::note_github_command_runner_call();
        let context = self
            .context
            .clone()
            .unwrap_or_else(|| self.default_context());
        let GhCall {
            timeout,
            args,
            #[cfg(all(test, unix))]
            spawn_receipt,
            #[cfg(test)]
            prepare_receipt,
            ..
        } = self;
        let timed_out = |stage| GhRunError::TimedOut {
            context: context.clone(),
            timeout,
            stage,
        };
        let deadline = tokio::time::Instant::now() + timeout;

        // Preparation runs no subprocess (in-process PATH lookup + vault
        // read), so abandoning it at the deadline leaves nothing running.
        let server = server.clone();
        let prepared = tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || {
                #[cfg(not(test))]
                {
                    build_gh_command(&server)
                }
                #[cfg(test)]
                {
                    let prepared = build_gh_command(&server);
                    if let Some(receipt) = prepare_receipt {
                        let _ = receipt.send(());
                    }
                    prepared
                }
            }),
        )
        .await;
        let (cmd, token) = match prepared {
            Err(_) => return Err(timed_out(GhTimeoutStage::Prepare)),
            Ok(Err(join_error)) => {
                return Err(GhRunError::Prepare(format!(
                    "prepare `gh` command task failed: {join_error}"
                )))
            }
            Ok(Ok(Err(reason))) => return Err(GhRunError::Prepare(reason)),
            Ok(Ok(Ok(prepared))) => prepared,
        };

        let mut cmd = tokio::process::Command::from(cmd);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|error| GhRunError::Execute(error.to_string()))?;
        #[cfg(all(test, unix))]
        if let (Some(receipt), Some(pid)) = (spawn_receipt, child.id()) {
            receipt.send(pid);
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let finished =
            tokio::time::timeout_at(deadline, collect_child(&mut child, stdout, stderr)).await;
        match finished {
            Ok(Ok((status, stdout, stderr))) => Ok(GhOutput {
                status,
                stdout,
                stderr,
                token,
            }),
            Ok(Err(error)) => {
                kill_and_reap(&mut child).await;
                Err(GhRunError::Execute(error.to_string()))
            }
            Err(_) => {
                kill_and_reap(&mut child).await;
                Err(timed_out(GhTimeoutStage::Execute))
            }
        }
    }

    /// Successful stdout, sanitized and truncated to `MAX_GH_OUTPUT_CHARS`
    /// (the former `run_gh` contract).
    pub(crate) async fn run(self, server: &MemoryServer) -> Result<String, GhRunError> {
        let output = self.output(server).await?;
        if !output.success() {
            return Err(output.exit_error());
        }
        let stdout = output.sanitized_stdout();
        if stdout.len() > MAX_GH_OUTPUT_CHARS {
            let truncated: String = stdout.chars().take(MAX_GH_OUTPUT_CHARS).collect();
            return Ok(format!(
                "{}\n\n[truncated: {} total chars]",
                truncated,
                stdout.len()
            ));
        }
        Ok(stdout)
    }
}

async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> std::io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_end(&mut buffer).await?;
    }
    Ok(buffer)
}

async fn collect_child(
    child: &mut tokio::process::Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
) -> std::io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let (stdout, stderr, status) = tokio::join!(read_pipe(stdout), read_pipe(stderr), child.wait());
    Ok((status?, stdout?, stderr?))
}

/// Kill the child and wait for its exit status so it does not linger as a
/// zombie. Bounded: if the kernel does not report the exit within the grace
/// period, `kill_on_drop` and tokio's orphan reaper remain as the backstop.
async fn kill_and_reap(child: &mut tokio::process::Child) {
    // Err here means the child already exited and was reaped by `wait`.
    let _ = child.start_kill();
    if tokio::time::timeout(GH_REAP_GRACE, child.wait())
        .await
        .is_err()
    {
        tracing::warn!("gh child did not exit within {GH_REAP_GRACE:?} after SIGKILL");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Instant;

    struct PathEnvGuard(Option<OsString>);

    impl PathEnvGuard {
        fn prepend(dir: &Path) -> Self {
            let original = std::env::var_os("PATH");
            let mut paths = vec![dir.to_path_buf()];
            if let Some(value) = original.as_ref() {
                paths.extend(std::env::split_paths(value));
            }
            std::env::set_var("PATH", std::env::join_paths(paths).expect("join PATH"));
            Self(original)
        }
    }

    impl Drop for PathEnvGuard {
        fn drop(&mut self) {
            match self.0.as_ref() {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    fn write_executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write shim");
        let mut permissions = std::fs::metadata(path)
            .expect("shim metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod shim");
    }

    /// `gh` shim that records its pid, then becomes a long sleep (same pid).
    fn install_sleeping_gh(dir: &Path, pid_file: &Path) {
        write_executable(
            &dir.join("gh"),
            &format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
                pid_file.display()
            ),
        );
    }

    /// True when no process (not even a zombie) has this pid any more.
    fn pid_gone(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 performs only the existence/permission check.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    /// Failure-only cleanup; successful assertions must observe Tokio's cleanup first.
    struct FixtureChildCleanup(Option<libc::pid_t>);

    impl Drop for FixtureChildCleanup {
        fn drop(&mut self) {
            let Some(pid) = self.0 else { return };
            let mut status = 0;
            // SAFETY: this PID came from this test's spawn receipt. Only kill a
            // child still owned by this process; ECHILD cannot authorize a kill.
            let running = loop {
                let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break rc == 0;
            };
            if running && unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
                // SAFETY: wait only for the owned fixture child just killed.
                while unsafe { libc::waitpid(pid, &mut status, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                }
            }
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
    }

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Exercise the real child and timeout cleanup, but resume the Tokio clock
    /// only after Rust has spawned the child. A blocking keeper prevents automatic
    /// clock advancement while waiting for the spawn receipt.
    /// Production still uses its single unmodified prepare/execute deadline.
    async fn run_sleeping_call(
        mut call: GhCall,
        server: &MemoryServer,
        cleanup: &mut FixtureChildCleanup,
    ) -> (Result<String, GhRunError>, libc::pid_t) {
        tokio::time::pause();
        let (spawn_tx, spawn_rx) = std::sync::mpsc::channel();
        call.spawn_receipt = Some(SpawnReceipt::Blocking(spawn_tx));
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let keeper = tokio::task::spawn_blocking(move || {
            let pid = spawn_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("gh child spawn receipt before readiness deadline");
            let cleanup = FixtureChildCleanup(Some(pid as libc::pid_t));
            ready_tx
                .send((pid, cleanup))
                .unwrap_or_else(|_| panic!("PID receiver"));
            // Also bounded if the async side fails. Dropping release_tx on a
            // panic wakes this thread immediately rather than stranding it.
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        });
        let result = call.run(server);
        tokio::pin!(result);
        let (pid, receipt_cleanup) = tokio::select! {
            result = &mut result => panic!("sleeping gh finished before PID readiness: {result:?}"),
            receipt = ready_rx => receipt.expect("child spawned before clock advance"),
        };
        *cleanup = receipt_cleanup;
        // Use the real timer for execution and kill/reap. Advancing a paused
        // clock exactly to a deadline can leave its rounded timer tick pending
        // while the keeper correctly prevents automatic advancement.
        tokio::time::resume();
        release_tx.send(()).expect("release clock keeper");
        keeper.await.expect("join clock keeper");
        (result.await, pid as libc::pid_t)
    }

    #[test]
    fn sleeping_gh_read_times_out_typed_and_child_is_killed_and_reaped() {
        let _lock = lock();
        let bin = tempfile::tempdir().expect("bin dir");
        // Spawn readiness must not depend on the child reaching a shell command.
        write_executable(&bin.path().join("gh"), "#!/bin/sh\nexec sleep 30\n");
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();

        let started = Instant::now();
        let rt = runtime();
        let mut cleanup = FixtureChildCleanup(None);
        let (result, pid) = rt.block_on(async {
            let mut call = GhCall::read();
            call.args(["issue", "view", "7"])
                .timeout(Duration::from_millis(1500));
            run_sleeping_call(call, &server, &mut cleanup).await
        });
        let error = result.expect_err("a sleeping gh must time out");
        let elapsed = started.elapsed();

        assert!(
            matches!(
                error,
                GhRunError::TimedOut {
                    stage: GhTimeoutStage::Execute,
                    ..
                }
            ),
            "typed execute-stage read timeout expected, got {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.starts_with("gh issue view timed out after"),
            "{message}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "bounded call must return near its deadline, took {elapsed:?}"
        );
        assert!(
            pid_gone(pid),
            "the timed-out gh child {pid} must be killed and reaped, not left running or as a zombie"
        );
        cleanup.0 = None;
    }

    /// A caller-side cancellation (the pattern that could not work while the
    /// runner blocked the executor thread) now regains control promptly.
    #[test]
    fn caller_timeout_on_current_thread_runtime_is_no_longer_blocked() {
        let _lock = lock();
        let bin = tempfile::tempdir().expect("bin dir");
        let pid_file = bin.path().join("gh.pid");
        // This child deliberately never writes a PID file. Its spawn receipt
        // proves process creation independently of shell startup/readiness.
        write_executable(&bin.path().join("gh"), "#!/bin/sh\nexec sleep 30\n");
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();

        let started = Instant::now();
        let rt = runtime();
        let mut cleanup = FixtureChildCleanup(None);
        let (pid, outcome) = rt.block_on(async {
            let (spawn_tx, spawn_rx) = tokio::sync::oneshot::channel();
            let mut call = GhCall::read();
            call.args(["pr", "view", "42"]);
            call.spawn_receipt = Some(SpawnReceipt::Async(spawn_tx));
            let mut execution = Box::pin(call.run(&server));
            let pid = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    pid = spawn_rx => pid.expect("gh child spawn receipt"),
                    _ = &mut execution => panic!("gh call finished before child spawn receipt"),
                }
            })
            .await
            .expect("gh child spawned before caller cancellation window");
            // Timeout owns the boxed execution future, so expiration drops the
            // actual Child instead of dropping only a borrowed pinned future.
            let outcome = tokio::time::timeout(Duration::from_millis(1500), execution).await;
            (pid as libc::pid_t, outcome)
        });
        cleanup.0 = Some(pid);
        assert!(outcome.is_err(), "the caller's own timeout must fire");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "caller regained control after {:?}",
            started.elapsed()
        );
        assert!(
            !pid_file.exists(),
            "fixture must not depend on a shell PID file"
        );
        // Keep driving the runtime's orphan reaper. ESRCH proves exit on every
        // Unix host, including macOS where /proc is unavailable.
        let deadline = Instant::now() + Duration::from_secs(5);
        rt.block_on(async {
            while !pid_gone(pid) {
                assert!(
                    Instant::now() < deadline,
                    "dropped gh child {pid} still running"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        cleanup.0 = None;
    }

    #[test]
    fn caller_timeout_during_queued_prepare_spawns_no_child() {
        let _lock = lock();
        let bin = tempfile::tempdir().expect("bin dir");
        let pid_file = bin.path().join("gh.pid");
        install_sleeping_gh(bin.path(), &pid_file);
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();
        let rt = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("single blocking worker runtime");
        rt.block_on(async {
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered_tx.send(()).expect("blocker entered");
                std::thread::sleep(Duration::from_secs(2));
                // Keep prepare queued until cancellation is observed. Unwinding
                // also drops release_tx, so failure cannot strand this worker.
                let _ = release_rx.recv();
            });
            entered_rx.await.expect("blocking slot occupied");
            let (spawn_tx, mut spawn_rx) = tokio::sync::oneshot::channel();
            let (prepare_tx, mut prepare_rx) = tokio::sync::oneshot::channel();
            let mut call = GhCall::read();
            call.args(["pr", "view", "42"]);
            call.spawn_receipt = Some(SpawnReceipt::Async(spawn_tx));
            call.prepare_receipt = Some(prepare_tx);
            let started = Instant::now();
            let outcome =
                tokio::time::timeout(Duration::from_millis(1500), call.run(&server)).await;
            assert!(
                outcome.is_err(),
                "caller cancellation must fire during queued prepare"
            );
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "caller must regain control"
            );
            assert!(matches!(
                prepare_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert!(matches!(
                spawn_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ));
            assert!(
                !pid_file.exists(),
                "queued prepare cannot have spawned a child"
            );
            release_tx.send(()).expect("release blocking worker");
            blocker.await.expect("blocker exits");
            tokio::time::timeout(Duration::from_secs(5), &mut prepare_rx)
                .await
                .expect("abandoned preparation finishes")
                .expect("preparation completion receipt");
            assert!(matches!(
                spawn_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ));
            assert!(
                !pid_file.exists(),
                "abandoned preparation must not spawn a child later"
            );
        });
    }

    #[test]
    fn fast_gh_success_and_exit_failure_keep_old_contracts() {
        let _lock = lock();
        let bin = tempfile::tempdir().expect("bin dir");
        write_executable(
            &bin.path().join("gh"),
            "#!/bin/sh\nif [ \"$1\" = fail ]; then echo 'boom' >&2; exit 3; fi\nprintf '{\"ok\":true}'\n",
        );
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();
        let rt = runtime();

        let ok = rt
            .block_on(async {
                let mut call = GhCall::read();
                call.args(["ok"]);
                call.run(&server).await
            })
            .expect("fast gh succeeds");
        assert_eq!(ok, "{\"ok\":true}");

        let error = rt
            .block_on(async {
                let mut call = GhCall::read();
                call.args(["fail"]);
                call.run(&server).await
            })
            .expect_err("non-zero exit is an error");
        assert!(
            matches!(error, GhRunError::Exit { code: 3, .. }),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "gh failed (exit 3): boom\n");
    }

    /// Preparation spawns nothing: `gh` is located in-process, never through
    /// a `which` subprocess, so a deadline hit during preparation cannot
    /// leave a command running.
    #[test]
    fn gh_path_lookup_is_in_process_and_honors_path_order() {
        let _lock = lock();
        let first = tempfile::tempdir().expect("first dir");
        let second = tempfile::tempdir().expect("second dir");
        let which_log = first.path().join("which.log");
        write_executable(
            &first.path().join("which"),
            &format!(
                "#!/bin/sh\necho called >> '{}'\nexit 1\n",
                which_log.display()
            ),
        );
        // Non-executable `gh` earlier on PATH is skipped, as `which` does.
        std::fs::write(first.path().join("gh"), "not executable").expect("plain file");
        write_executable(&second.path().join("gh"), "#!/bin/sh\nexit 0\n");
        let _second = PathEnvGuard::prepend(second.path());
        let _first = PathEnvGuard::prepend(first.path());

        let resolved = super::super::resolve_gh_path().expect("resolve gh");
        assert_eq!(Path::new(&resolved), second.path().join("gh"));
        assert!(
            !which_log.exists(),
            "resolve_gh_path must not spawn `which`"
        );
    }
}
