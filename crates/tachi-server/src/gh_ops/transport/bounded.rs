//! The one `gh` subprocess executor (audit E1).
//!
//! Every `gh` invocation in `gh_ops` goes through [`GhCall`]. One deadline
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
//! A timed-out MUTATION is reported with an UNKNOWN outcome: the request may
//! already have reached GitHub. The executor never retries.

use super::{
    build_gh_command, sanitize_output, Duration, GhBodyFileGuard, MemoryServer, MAX_GH_OUTPUT_CHARS,
};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use tokio::io::AsyncReadExt;

/// Default bound for a single-object `gh` read (`issue view`, `pr view`,
/// `pr checks`, a short `list`).
pub(in crate::gh_ops) const GH_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound for bulk or paginated reads (`--limit 500` lists, `api --paginate`).
pub(in crate::gh_ops) const GH_BULK_READ_TIMEOUT: Duration = Duration::from_secs(180);
/// Bound for a `gh` command that changes remote state. On expiry the outcome
/// is UNKNOWN, never assumed failed or succeeded.
pub(in crate::gh_ops) const GH_MUTATION_TIMEOUT: Duration = Duration::from_secs(120);
/// How long to wait for the kernel to report a killed child's exit.
const GH_REAP_GRACE: Duration = Duration::from_secs(5);
/// Stderr kept in an exit-failure diagnostic (same limit as the old runner).
const GH_STDERR_DIAGNOSTIC_CHARS: usize = 1000;

/// Whether a `gh` invocation can change remote state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gh_ops) enum GhEffect {
    Read,
    Mutation,
}

/// Where the deadline fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gh_ops) enum GhTimeoutStage {
    /// Before any `gh` process was spawned. Nothing reached GitHub.
    Prepare,
    /// While the spawned `gh` process was running. It has been killed.
    Execute,
}

/// Typed failure of a bounded `gh` call.
#[derive(Debug)]
pub(in crate::gh_ops) enum GhRunError {
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
        effect: GhEffect,
        stage: GhTimeoutStage,
    },
}

#[cfg(test)]
impl GhRunError {
    /// A mutation whose `gh` process was killed mid-flight: GitHub may or
    /// may not have applied it.
    pub(in crate::gh_ops) fn outcome_unknown(&self) -> bool {
        matches!(
            self,
            Self::TimedOut {
                effect: GhEffect::Mutation,
                stage: GhTimeoutStage::Execute,
                ..
            }
        )
    }
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
                effect,
                stage,
            } => match (effect, stage) {
                (_, GhTimeoutStage::Prepare) => write!(
                    f,
                    "{context} timed out after {timeout:?} while preparing the gh command; \
                     no gh process was started"
                ),
                (GhEffect::Read, GhTimeoutStage::Execute) => write!(
                    f,
                    "{context} timed out after {timeout:?}; the gh process was killed"
                ),
                (GhEffect::Mutation, GhTimeoutStage::Execute) => write!(
                    f,
                    "{context} timed out after {timeout:?}; the gh process was killed and the \
                     outcome is UNKNOWN (GitHub may or may not have applied the change). \
                     Verify the remote state before retrying; this call is never retried \
                     automatically"
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
pub(in crate::gh_ops) struct GhOutput {
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
    pub(in crate::gh_ops) fn success(&self) -> bool {
        self.status.success()
    }

    pub(in crate::gh_ops) fn sanitized_stdout(&self) -> String {
        sanitize_output(&String::from_utf8_lossy(&self.stdout), &self.token)
    }

    pub(in crate::gh_ops) fn sanitized_stderr(&self) -> String {
        sanitize_output(&String::from_utf8_lossy(&self.stderr), &self.token)
    }

    /// Raw parts for callers that must inspect unsanitized bytes internally
    /// (see `bounded_json::decode_output`).
    pub(in crate::gh_ops) fn into_parts(self) -> (std::process::Output, String) {
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
pub(in crate::gh_ops) struct GhCall {
    effect: GhEffect,
    timeout: Duration,
    context: Option<String>,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
    body_files: Vec<GhBodyFileGuard>,
}

impl GhCall {
    fn new(effect: GhEffect, timeout: Duration) -> Self {
        Self {
            effect,
            timeout,
            context: None,
            args: Vec::new(),
            current_dir: None,
            body_files: Vec::new(),
        }
    }

    /// A read that cannot change remote state, bounded by [`GH_READ_TIMEOUT`].
    pub(in crate::gh_ops) fn read() -> Self {
        Self::new(GhEffect::Read, GH_READ_TIMEOUT)
    }

    /// A bulk or paginated read, bounded by [`GH_BULK_READ_TIMEOUT`].
    pub(in crate::gh_ops) fn bulk_read() -> Self {
        Self::new(GhEffect::Read, GH_BULK_READ_TIMEOUT)
    }

    /// A remote-state change, bounded by [`GH_MUTATION_TIMEOUT`].
    pub(in crate::gh_ops) fn mutation() -> Self {
        Self::new(GhEffect::Mutation, GH_MUTATION_TIMEOUT)
    }

    pub(in crate::gh_ops) fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub(in crate::gh_ops) fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    pub(in crate::gh_ops) fn current_dir(&mut self, dir: impl Into<PathBuf>) -> &mut Self {
        self.current_dir = Some(dir.into());
        self
    }

    pub(in crate::gh_ops) fn timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = timeout;
        self
    }

    /// Label used in timeout diagnostics. Defaults to `gh <arg0> <arg1>`.
    pub(in crate::gh_ops) fn context(&mut self, context: impl Into<String>) -> &mut Self {
        self.context = Some(context.into());
        self
    }

    /// Write `body` to a per-call temp file and append `--body-file <path>`.
    /// The file lives until this call has finished (or been killed).
    pub(in crate::gh_ops) fn attach_body_file(&mut self, body: &str) -> Result<&mut Self, String> {
        let guard = super::write_gh_body_file(body)?;
        let path = guard
            .0
            .to_str()
            .ok_or_else(|| "gh body tempfile path is not valid UTF-8".to_string())?
            .to_string();
        self.body_files.push(guard);
        self.args(["--body-file", path.as_str()]);
        Ok(self)
    }

    #[cfg(test)]
    pub(in crate::gh_ops) fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.args.iter().map(OsString::as_os_str)
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
    pub(in crate::gh_ops) async fn output(
        self,
        server: &MemoryServer,
    ) -> Result<GhOutput, GhRunError> {
        #[cfg(test)]
        super::note_github_command_runner_call();
        let context = self
            .context
            .clone()
            .unwrap_or_else(|| self.default_context());
        let GhCall {
            effect,
            timeout,
            args,
            current_dir,
            body_files,
            ..
        } = self;
        let timed_out = |stage| GhRunError::TimedOut {
            context: context.clone(),
            timeout,
            effect,
            stage,
        };
        let deadline = tokio::time::Instant::now() + timeout;

        // Preparation runs no subprocess (in-process PATH lookup + vault
        // read), so abandoning it at the deadline leaves nothing running.
        let server = server.clone();
        let prepared = tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || build_gh_command(&server)),
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
        if let Some(dir) = &current_dir {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .spawn()
            .map_err(|error| GhRunError::Execute(error.to_string()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let finished =
            tokio::time::timeout_at(deadline, collect_child(&mut child, stdout, stderr)).await;
        let result = match finished {
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
        };
        // Body files outlive the child: only removed once it is gone.
        drop(body_files);
        result
    }

    /// Successful stdout, sanitized and truncated to `MAX_GH_OUTPUT_CHARS`
    /// (the former `run_gh` contract).
    pub(in crate::gh_ops) async fn run(self, server: &MemoryServer) -> Result<String, GhRunError> {
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

    /// Successful stdout, sanitized and NOT truncated, for JSON parsing (the
    /// former `run_gh_json` contract).
    pub(in crate::gh_ops) async fn run_json(
        self,
        server: &MemoryServer,
    ) -> Result<String, GhRunError> {
        let output = self.output(server).await?;
        if !output.success() {
            return Err(output.exit_error());
        }
        Ok(output.sanitized_stdout())
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

    fn read_pid(pid_file: &Path) -> libc::pid_t {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = std::fs::read_to_string(pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "gh shim never wrote its pid");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// True when no process (not even a zombie) has this pid any more.
    fn pid_gone(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 performs only the existence/permission check.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
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

    #[test]
    fn sleeping_gh_read_times_out_typed_and_child_is_killed_and_reaped() {
        let _lock = lock();
        let bin = tempfile::tempdir().expect("bin dir");
        let pid_file = bin.path().join("gh.pid");
        install_sleeping_gh(bin.path(), &pid_file);
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();

        let started = Instant::now();
        let error = runtime()
            .block_on(async {
                let mut call = GhCall::read();
                call.args(["issue", "view", "7"])
                    .timeout(Duration::from_millis(1500));
                call.run(&server).await
            })
            .expect_err("a sleeping gh must time out");
        let elapsed = started.elapsed();

        assert!(
            matches!(
                error,
                GhRunError::TimedOut {
                    effect: GhEffect::Read,
                    stage: GhTimeoutStage::Execute,
                    ..
                }
            ),
            "typed execute-stage read timeout expected, got {error:?}"
        );
        assert!(!error.outcome_unknown(), "a read is never outcome-unknown");
        let message = error.to_string();
        assert!(
            message.starts_with("gh issue view timed out after"),
            "{message}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "bounded call must return near its deadline, took {elapsed:?}"
        );
        let pid = read_pid(&pid_file);
        assert!(
            pid_gone(pid),
            "the timed-out gh child {pid} must be killed and reaped, not left running or as a zombie"
        );
    }

    #[test]
    fn sleeping_gh_mutation_timeout_is_outcome_unknown_and_body_file_removed() {
        let _lock = lock();
        let bin = tempfile::tempdir().expect("bin dir");
        let pid_file = bin.path().join("gh.pid");
        install_sleeping_gh(bin.path(), &pid_file);
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();

        let (error, body_file) = runtime().block_on(async {
            let mut call = GhCall::mutation();
            call.args(["issue", "comment", "7", "--repo", "owner/repo"])
                .timeout(Duration::from_millis(1500));
            call.attach_body_file("body").expect("attach body");
            let body_file = call
                .get_args()
                .last()
                .map(|arg| std::path::PathBuf::from(arg))
                .expect("body file arg");
            assert!(body_file.exists());
            (
                call.run(&server).await.expect_err("must time out"),
                body_file,
            )
        });

        assert!(error.outcome_unknown(), "got {error:?}");
        let message = String::from(error);
        assert!(message.contains("outcome is UNKNOWN"), "{message}");
        assert!(message.contains("never retried"), "{message}");
        assert!(
            pid_gone(read_pid(&pid_file)),
            "mutation child must be reaped"
        );
        assert!(
            !body_file.exists(),
            "body file is removed after the killed call"
        );
    }

    /// A caller-side cancellation (the pattern that could not work while the
    /// runner blocked the executor thread) now regains control promptly.
    #[test]
    fn caller_timeout_on_current_thread_runtime_is_no_longer_blocked() {
        let _lock = lock();
        let bin = tempfile::tempdir().expect("bin dir");
        let pid_file = bin.path().join("gh.pid");
        install_sleeping_gh(bin.path(), &pid_file);
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();

        let started = Instant::now();
        let outcome = runtime().block_on(async {
            let mut call = GhCall::read();
            call.args(["pr", "view", "42"]);
            tokio::time::timeout(Duration::from_millis(1500), call.run(&server)).await
        });
        assert!(outcome.is_err(), "the caller's own timeout must fire");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "caller regained control after {:?}",
            started.elapsed()
        );
        // Dropping the call future kills the child (kill_on_drop backstop);
        // the runtime is gone, so only check that it is no longer running.
        let pid = read_pid(&pid_file);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
            let running = state.as_deref().is_some_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| !rest.starts_with('Z'))
            });
            if !running || !Path::new("/proc").exists() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "dropped gh child {pid} still running"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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
                call.arg("ok");
                call.run_json(&server).await
            })
            .expect("fast gh succeeds");
        assert_eq!(ok, "{\"ok\":true}");

        let error = rt
            .block_on(async {
                let mut call = GhCall::read();
                call.arg("fail");
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
