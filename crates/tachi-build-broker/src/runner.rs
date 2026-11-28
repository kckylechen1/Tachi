//! The executor seat's hands: checkout, run, wipe (#894 S2c item 3).
//!
//! Behind a trait for two reasons. One, the broker's *decisions* (serialize,
//! pick a target, quarantine on interrupt) are the part worth testing, and they
//! must be testable without a Rust toolchain in the loop. Two, the real
//! implementation is three subprocesses and an `rm -rf`, which is exactly the
//! kind of code that should be a leaf, not a dependency of the policy.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::ticket::{BuildCommand, BuildTicket, SourceIdentity};

/// How a build ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildOutcome {
    /// Exit 0.
    Success,
    /// Non-zero exit — a real compile/test failure. The target dir is in a
    /// well-defined state (cargo finished writing) and keeps its generation.
    Failed,
    /// Killed by a signal / never reached an exit status. The target dir was
    /// being written to when the process died: its contents are NOT trustworthy
    /// and it gets quarantined (#894 S2c item 4).
    Interrupted,
}

/// What one build run produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRun {
    pub outcome: BuildOutcome,
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// The seat's hands.
pub trait BuildRunner {
    /// Put the seat's fixed checkout on the ticket's source commit.
    ///
    /// MUST refuse a dirty checkout: the seat's tree is a shared machine
    /// resource, and checking out over someone's uncommitted work is the
    /// clobber this whole design exists to avoid.
    fn prepare_checkout(&self, checkout: &Path, source: &SourceIdentity) -> Result<(), String>;

    /// Run the ticket's command in `checkout` with `CARGO_TARGET_DIR=target_dir`.
    fn run(
        &self,
        ticket: &BuildTicket,
        checkout: &Path,
        target_dir: &Path,
    ) -> Result<BuildRun, String>;

    /// Wipe a target dir. Returns the bytes freed. Must be idempotent (wiping an
    /// already-empty/absent dir frees 0 and is not an error).
    fn clear_target(&self, target_dir: &Path) -> Result<i64, String>;
}

/// The build runs the ticket's source. That code is not trusted with this
/// process's credentials, and it must not mistake itself for the daemon.
fn build_command(
    command: &BuildCommand,
    checkout: &Path,
    target_dir: &Path,
) -> std::process::Command {
    let mut build = std::process::Command::new(&command.program);
    build
        .args(&command.args)
        .current_dir(checkout)
        .env("CARGO_TARGET_DIR", target_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    memcore::child_env::withhold_inherited_credentials(&mut build);
    build
}

/// The real seat: git + a subprocess + `remove_dir_all`.
pub struct ProcessBuildRunner;

impl BuildRunner for ProcessBuildRunner {
    fn prepare_checkout(&self, checkout: &Path, source: &SourceIdentity) -> Result<(), String> {
        let dirty = git_stdout(checkout, &["status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(format!(
                "executor seat checkout '{}' is dirty; refusing to check out {} over it — the \
                 seat holds ONE fixed clean checkout and never clobbers work in it (#894 S2c)",
                checkout.display(),
                source.head_sha
            ));
        }
        // `--detach <sha>`: the seat never sits on a branch, so it can never
        // "just pull" or drift. The sha was validated as a hex object id by
        // `BuildTicket::new`, so it cannot be a flag or a ref name.
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["checkout", "--detach", &source.head_sha])
            .status()
            .map_err(|e| format!("git checkout: {e}"))?;
        if !status.success() {
            return Err(format!(
                "git checkout --detach {} in '{}' failed ({status})",
                source.head_sha,
                checkout.display()
            ));
        }
        // Verify we landed where the ticket said. A silent mismatch here would
        // build the wrong tree and attribute it to this ticket.
        let head = git_stdout(checkout, &["rev-parse", "HEAD"])?;
        let head = head.trim();
        if !head.starts_with(&source.head_sha) && !source.head_sha.starts_with(head) {
            return Err(format!(
                "executor seat HEAD is {head} after checking out {}: refusing to build a tree \
                 that is not the ticket's source (#894 S2c)",
                source.head_sha
            ));
        }
        Ok(())
    }

    fn run(
        &self,
        ticket: &BuildTicket,
        checkout: &Path,
        target_dir: &Path,
    ) -> Result<BuildRun, String> {
        std::fs::create_dir_all(target_dir)
            .map_err(|e| format!("create target dir {}: {e}", target_dir.display()))?;
        run_process(ticket, checkout, target_dir, None)
    }

    fn clear_target(&self, target_dir: &Path) -> Result<i64, String> {
        if !target_dir.exists() {
            return Ok(0);
        }
        let bytes = dir_size(target_dir);
        std::fs::remove_dir_all(target_dir)
            .map_err(|e| format!("clear target dir {}: {e}", target_dir.display()))?;
        std::fs::create_dir_all(target_dir)
            .map_err(|e| format!("recreate target dir {}: {e}", target_dir.display()))?;
        Ok(bytes)
    }
}

const ROOT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const OUTPUT_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const GROUP_REAP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
enum CaptureFault {
    #[cfg(test)]
    ReadFailure {
        stream: &'static str,
        ready: std::path::PathBuf,
    },
    #[cfg(test)]
    WithholdResults,
}

fn run_process(
    ticket: &BuildTicket,
    checkout: &Path,
    target_dir: &Path,
    capture_fault: Option<CaptureFault>,
) -> Result<BuildRun, String> {
    let (stdout_tx, stdout_rx) = mpsc::sync_channel::<ChildStdout>(1);
    let (stderr_tx, stderr_rx) = mpsc::sync_channel::<ChildStderr>(1);
    let (result_tx, result_rx) = mpsc::channel();
    let (exit_tx, exit_rx) = mpsc::channel();
    let stdout_exit = exit_tx.clone();
    let stdout_result = result_tx.clone();
    let stdout_fault = capture_fault.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let stdout_cancel = cancel.clone();
    let stderr_cancel = cancel.clone();
    // Readers are deliberately not scoped: a descendant outside our process
    // group can keep a pipe open forever, and joining such a reader would turn
    // the bounded post-reap wait below into a fictitious timeout.
    let _stdout_reader = std::thread::Builder::new()
        .name("build-stdout".into())
        .spawn(move || {
            drain_runner_pipe(
                stdout_rx,
                stdout_result,
                "stdout",
                stdout_fault,
                stdout_cancel,
                stdout_exit,
            );
        })
        .map_err(|e| format!("start stdout reader: {e}"))?;
    let _stderr_reader = std::thread::Builder::new()
        .name("build-stderr".into())
        .spawn(move || {
            drain_runner_pipe(
                stderr_rx,
                result_tx,
                "stderr",
                capture_fault,
                stderr_cancel,
                exit_tx,
            );
        })
        .map_err(|e| format!("start stderr reader: {e}"))?;

    let mut command = build_command(&ticket.command, checkout, target_dir);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The direct child leads the only group this runner will ever terminate.
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", ticket.command.program))?;
    run_spawned(
        child,
        &ticket.command.program,
        stdout_tx,
        stderr_tx,
        result_rx,
        exit_rx,
        cancel,
    )
}

fn run_spawned(
    child: Child,
    program: &str,
    stdout_tx: mpsc::SyncSender<ChildStdout>,
    stderr_tx: mpsc::SyncSender<ChildStderr>,
    result_rx: mpsc::Receiver<(&'static str, Result<String, String>)>,
    exit_rx: mpsc::Receiver<&'static str>,
    cancel: Arc<AtomicBool>,
) -> Result<BuildRun, String> {
    let _cancel_on_drop = ReaderCancelOnDrop(cancel.clone());
    let mut child = OwnedChild::new(child);
    let mut capture = OutputCapture::default();

    // No error returns before the common termination/reap path below. Both
    // senders leave scope here even when one handoff fails, waking both readers.
    let handoff = (|| {
        stdout_tx
            .send(
                child
                    .child
                    .stdout
                    .take()
                    .ok_or("missing build stdout pipe")?,
            )
            .map_err(|_| "build stdout reader stopped")?;
        stderr_tx
            .send(
                child
                    .child
                    .stderr
                    .take()
                    .ok_or("missing build stderr pipe")?,
            )
            .map_err(|_| "build stderr reader stopped")?;
        Ok::<(), String>(())
    })();
    if let Err(error) = handoff {
        capture.fail(error);
    }

    let mut root_exited = false;
    while capture.error.is_none() && !root_exited {
        match child.observe_exit() {
            Ok(exited) => root_exited = exited,
            Err(error) => {
                capture.fail(format!("observe {program}: {error}"));
                break;
            }
        }
        if root_exited {
            break;
        }
        if capture.complete() {
            std::thread::sleep(ROOT_POLL_INTERVAL);
            continue;
        }
        match result_rx.recv_timeout(ROOT_POLL_INTERVAL) {
            Ok((stream, result)) => capture.record(stream, result),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                capture.fail("build output reader stopped without a result".into());
            }
        }
    }

    // WNOWAIT kept an exited root as a zombie, so its numeric PGID cannot be
    // recycled. This is the LAST termination signal: it is sent before the one
    // reap below, and OwnedChild refuses all termination after that reap.
    let termination_error = child.terminate();
    let status = child.reap();
    let group_absent = child.confirm_group_absent();
    // Drain both tails after reaping, but never join a reader. A setsid escape
    // is not contained by the group kill; if it retains a pipe, this bounded
    // wait turns the run into an error (and therefore a broker quarantine).
    capture.collect_remaining(&result_rx, OUTPUT_REAP_TIMEOUT);
    // Wake nonblocking readers, then give them one short final turn to exit
    // and close their descriptors. An escaped pipe holder cannot make this
    // wait unbounded or leave a thread blocked in read().
    cancel.store(true, Ordering::Release);
    capture.collect_remaining(&result_rx, ROOT_POLL_INTERVAL * 10);
    wait_reader_exits(&exit_rx, ROOT_POLL_INTERVAL * 10)?;

    if let Some(error) = capture.error {
        return Err(error);
    }
    if let Some(error) = termination_error {
        // Darwin can report EPERM for a group containing only an unreaped zombie
        // leader. Successful reap plus signal-0 absence proves that case benign.
        #[cfg(target_os = "macos")]
        let benign_darwin_zombie =
            root_exited && error.raw_os_error() == Some(1) && matches!(&group_absent, Ok(true));
        #[cfg(not(target_os = "macos"))]
        let benign_darwin_zombie = false;
        if !benign_darwin_zombie {
            return Err(format!("terminate {program} process group: {error}"));
        }
    }
    match group_absent {
        Ok(true) => {}
        Ok(false) => {
            return Err(format!(
                "{program} process group still exists after root reap; quarantining target"
            ));
        }
        Err(error) => {
            return Err(format!("confirm {program} process group cleanup: {error}"));
        }
    }
    let status = status.map_err(|e| format!("wait for {program}: {e}"))?;
    let outcome = match status.code() {
        Some(0) => BuildOutcome::Success,
        Some(_) => BuildOutcome::Failed,
        None => BuildOutcome::Interrupted,
    };
    Ok(BuildRun {
        outcome,
        exit_code: status.code(),
        stdout_tail: capture
            .stdout_tail
            .ok_or("build stdout reader stopped without a result")?,
        stderr_tail: capture
            .stderr_tail
            .ok_or("build stderr reader stopped without a result")?,
    })
}

#[derive(Default)]
struct OutputCapture {
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    stdout_done: bool,
    stderr_done: bool,
    error: Option<String>,
}

impl OutputCapture {
    fn complete(&self) -> bool {
        self.stdout_done && self.stderr_done
    }

    fn fail(&mut self, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    fn record(&mut self, stream: &'static str, result: Result<String, String>) {
        let done = if stream == "stdout" {
            &mut self.stdout_done
        } else {
            &mut self.stderr_done
        };
        if *done {
            self.fail(format!(
                "build {stream} reader returned more than one result"
            ));
            return;
        }
        *done = true;
        match result {
            Ok(output) if stream == "stdout" => self.stdout_tail = Some(output),
            Ok(output) => self.stderr_tail = Some(output),
            Err(error) => self.fail(error),
        }
    }

    fn collect_remaining(
        &mut self,
        results: &mpsc::Receiver<(&'static str, Result<String, String>)>,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        while !self.complete() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.fail(
                    "build output readers did not close after root reap; a descendant outside \
                     the owned process group may still hold a pipe"
                        .into(),
                );
                break;
            }
            match results.recv_timeout(remaining) {
                Ok((stream, result)) => self.record(stream, result),
                Err(RecvTimeoutError::Timeout) => {
                    self.fail(
                        "build output readers did not close after root reap; a descendant outside \
                         the owned process group may still hold a pipe"
                            .into(),
                    );
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.fail("build output reader stopped without a result".into());
                    break;
                }
            }
        }
    }
}

struct ReaderExitGuard<'a> {
    tx: &'a mpsc::Sender<&'static str>,
    stream: &'static str,
}

impl<'a> ReaderExitGuard<'a> {
    fn new(tx: &'a mpsc::Sender<&'static str>, stream: &'static str) -> ReaderExitGuard<'a> {
        Self { tx, stream }
    }
}

impl Drop for ReaderExitGuard<'_> {
    fn drop(&mut self) {
        let _ = self.tx.send(self.stream);
    }
}

fn wait_reader_exits(
    exit_rx: &mpsc::Receiver<&'static str>,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut exited = std::collections::HashSet::new();
    while exited.len() < 2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("build output readers did not exit after cancellation".to_string());
        }
        match exit_rx.recv_timeout(remaining) {
            Ok(stream) => {
                exited.insert(stream);
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err("build output readers did not exit after cancellation".to_string());
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("build output reader exited without a completion notification".into());
            }
        }
    }
    Ok(())
}

struct ReaderCancelOnDrop(Arc<AtomicBool>);

impl Drop for ReaderCancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct OwnedChild {
    child: Child,
    reaped: bool,
    #[cfg(unix)]
    pid: u32,
    #[cfg(unix)]
    probe_group: bool,
    #[cfg(not(unix))]
    early_status: Option<ExitStatus>,
}

impl OwnedChild {
    fn new(child: Child) -> Self {
        #[cfg(unix)]
        let pid = child.id();
        Self {
            child,
            reaped: false,
            #[cfg(unix)]
            pid,
            #[cfg(unix)]
            probe_group: true,
            #[cfg(not(unix))]
            early_status: None,
        }
    }

    #[cfg(unix)]
    fn observe_exit(&mut self) -> io::Result<bool> {
        if self.reaped {
            return Ok(true);
        }
        match unix::observe_root_exit(self.pid) {
            Ok(exited) => Ok(exited),
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    // Somebody else reaped the root. Its PID/PGID is no longer
                    // ours to signal or even probe as a retained identity.
                    self.reaped = true;
                    self.probe_group = false;
                }
                Err(error)
            }
        }
    }

    #[cfg(not(unix))]
    fn observe_exit(&mut self) -> io::Result<bool> {
        if self.reaped {
            return Ok(true);
        }
        if let Some(status) = self.child.try_wait()? {
            // Non-Unix has no WNOWAIT group-preservation protocol: try_wait
            // also reaps. The reaped state forbids every later kill attempt.
            self.early_status = Some(status);
            self.reaped = true;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    #[cfg(unix)]
    fn terminate(&mut self) -> Option<io::Error> {
        if self.reaped {
            return None;
        }
        match unix::signal_group(self.pid, libc::SIGKILL) {
            Ok(()) => None,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => None,
            Err(error) => {
                // The root is still unreaped, so a direct-child fallback cannot
                // hit a recycled PID. Any group error remains fatal below.
                let _ = self.child.kill();
                Some(error)
            }
        }
    }

    #[cfg(not(unix))]
    fn terminate(&mut self) -> Option<io::Error> {
        if self.reaped {
            return None;
        }
        self.child.kill().err()
    }

    fn reap(&mut self) -> io::Result<ExitStatus> {
        #[cfg(not(unix))]
        if self.reaped {
            return self.early_status.clone().ok_or_else(|| {
                io::Error::other("build root ownership was lost before final reap")
            });
        }
        if self.reaped {
            return Err(io::Error::other(
                "build root ownership was lost before final reap",
            ));
        }
        let status = self.child.wait();
        // std::process::Child has been asked to reap, including on an error. No
        // subsequent path is permitted to terminate its cached PID or PGID.
        self.reaped = true;
        status
    }

    #[cfg(unix)]
    fn confirm_group_absent(&self) -> io::Result<bool> {
        if !self.probe_group {
            return Ok(false);
        }
        let deadline = Instant::now() + GROUP_REAP_TIMEOUT;
        loop {
            if unix::group_absent(self.pid)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(ROOT_POLL_INTERVAL);
        }
    }

    #[cfg(not(unix))]
    fn confirm_group_absent(&self) -> io::Result<bool> {
        // There is no owned POSIX group to prove absent. Output collection is
        // still bounded, so an inherited pipe cannot hold the build slot.
        Ok(true)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // Panic/unwind fallback only. Normal success and every ordinary error
        // share run_spawned's single terminate -> reap -> collect path.
        #[cfg(unix)]
        let _ = unix::signal_group(self.pid, libc::SIGKILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reaped = true;
    }
}

#[cfg(unix)]
mod unix {
    use std::io;

    pub(super) fn observe_root_exit(pid: u32) -> io::Result<bool> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: info is a valid writable siginfo_t and WNOWAIT observes the
        // direct child without consuming its status or releasing its PGID.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }

    pub(super) fn signal_group(pid: u32, signal: i32) -> io::Result<()> {
        // SAFETY: the caller retains the unreaped group leader's identity.
        let result = unsafe { libc::kill(-(pid as i32), signal) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn group_absent(pid: u32) -> io::Result<bool> {
        match signal_group(pid, 0) {
            Ok(()) => Ok(false),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(true),
            Err(error) => Err(error),
        }
    }

    #[cfg(test)]
    pub(super) fn process_absent(pid: u32) -> bool {
        // SAFETY: signal 0 is an existence query and never terminates a process.
        let result = unsafe { libc::kill(pid as i32, 0) };
        result != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[cfg(test)]
    pub(super) fn enter_new_session_for_test() -> io::Result<()> {
        // SAFETY: only the isolated test helper enters a new session.
        if unsafe { libc::setsid() } == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub(super) fn kill_process_for_test(pid: u32) {
        // SAFETY: test-only last resort for a fixture that failed cleanup.
        let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}

#[cfg(unix)]
fn drain_pipe_cancellable<R: Read + std::os::fd::AsFd>(
    input: mpsc::Receiver<R>,
    result: mpsc::Sender<(&'static str, Result<String, String>)>,
    stream: &'static str,
    cancel: Arc<AtomicBool>,
) {
    let Ok(reader) = input.recv() else { return };
    drain_reader(reader, result, stream, cancel);
}

#[cfg(not(unix))]
fn drain_pipe_cancellable<R: Read>(
    input: mpsc::Receiver<R>,
    result: mpsc::Sender<(&'static str, Result<String, String>)>,
    stream: &'static str,
    cancel: Arc<AtomicBool>,
) {
    let Ok(reader) = input.recv() else { return };
    drain_reader(reader, result, stream, cancel);
}

#[cfg(test)]
fn drain_pipe<R: Read>(
    input: mpsc::Receiver<R>,
    result: mpsc::Sender<(&'static str, Result<String, String>)>,
    stream: &'static str,
) {
    let Ok(reader) = input.recv() else { return };
    let tail = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_output_tail(reader)))
        .map_err(|_| format!("build {stream} reader panicked"))
        .and_then(|r| r.map_err(|e| format!("read build {stream}: {e}")));
    let _ = result.send((stream, tail));
}

#[cfg(unix)]
fn drain_runner_pipe<R: Read + std::os::fd::AsFd>(
    input: mpsc::Receiver<R>,
    result: mpsc::Sender<(&'static str, Result<String, String>)>,
    stream: &'static str,
    fault: Option<CaptureFault>,
    cancel: Arc<AtomicBool>,
    exit_tx: mpsc::Sender<&'static str>,
) {
    let _exit = ReaderExitGuard::new(&exit_tx, stream);
    #[cfg(test)]
    if let Some(fault) = fault {
        match fault {
            CaptureFault::ReadFailure {
                stream: failed,
                ready,
            } if failed == stream => {
                let Ok(_reader) = input.recv() else { return };
                let deadline = Instant::now() + Duration::from_secs(5);
                while !ready.exists() && Instant::now() < deadline {
                    std::thread::sleep(ROOT_POLL_INTERVAL);
                }
                let _ = result.send((
                    stream,
                    Err(format!("read build {stream}: injected capture failure")),
                ));
                return;
            }
            CaptureFault::WithholdResults => {
                let Ok(reader) = input.recv() else { return };
                // Drain to EOF so both real senders drop naturally while the
                // root is still alive, producing an actual result disconnect.
                let _ = read_output_tail(reader);
                return;
            }
            _ => {}
        }
    }
    #[cfg(not(test))]
    let _ = fault;
    drain_pipe_cancellable(input, result, stream, cancel);
}

#[cfg(not(unix))]
fn drain_runner_pipe<R: Read>(
    input: mpsc::Receiver<R>,
    result: mpsc::Sender<(&'static str, Result<String, String>)>,
    stream: &'static str,
    fault: Option<CaptureFault>,
    cancel: Arc<AtomicBool>,
    exit_tx: mpsc::Sender<&'static str>,
) {
    let _exit = ReaderExitGuard::new(&exit_tx, stream);
    #[cfg(test)]
    if let Some(fault) = fault {
        match fault {
            CaptureFault::ReadFailure {
                stream: failed,
                ready,
            } if failed == stream => {
                let Ok(_reader) = input.recv() else { return };
                let deadline = Instant::now() + Duration::from_secs(5);
                while !ready.exists() && Instant::now() < deadline {
                    std::thread::sleep(ROOT_POLL_INTERVAL);
                }
                let _ = result.send((
                    stream,
                    Err(format!("read build {stream}: injected capture failure")),
                ));
                return;
            }
            CaptureFault::WithholdResults => {
                let Ok(reader) = input.recv() else { return };
                let _ = read_output_tail(reader);
                return;
            }
            _ => {}
        }
    }
    #[cfg(not(test))]
    let _ = fault;
    drain_pipe_cancellable(input, result, stream, cancel);
}

#[cfg(unix)]
#[cfg(unix)]
fn drain_reader<R: Read + std::os::fd::AsFd>(
    reader: R,
    result: mpsc::Sender<(&'static str, Result<String, String>)>,
    stream: &'static str,
    cancel: Arc<AtomicBool>,
) {
    let tail = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read_output_tail_with_cancel(reader, cancel)
    }))
    .map_err(|_| format!("build {stream} reader panicked"))
    .and_then(|r| r.map_err(|e| format!("read build {stream}: {e}")));
    let _ = result.send((stream, tail));
}

#[cfg(not(unix))]
fn drain_reader<R: Read>(
    reader: R,
    result: mpsc::Sender<(&'static str, Result<String, String>)>,
    stream: &'static str,
    _cancel: Arc<AtomicBool>,
) {
    let tail = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_output_tail(reader)))
        .map_err(|_| format!("build {stream} reader panicked"))
        .and_then(|r| r.map_err(|e| format!("read build {stream}: {e}")));
    let _ = result.send((stream, tail));
}

#[cfg(unix)]
fn read_output_tail_with_cancel<R: Read + std::os::fd::AsFd>(
    mut reader: R,
    cancel: Arc<AtomicBool>,
) -> io::Result<String> {
    use std::os::fd::AsRawFd;
    let fd = reader.as_fd().as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    const RETAIN: usize = TAIL_BYTES + 3;
    let mut retained = VecDeque::with_capacity(RETAIN);
    let mut buffer = [0; 8192];
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(ROOT_POLL_INTERVAL);
                continue;
            }
            Err(e) => return Err(e),
        };
        if read >= RETAIN {
            retained.clear();
            retained.extend(&buffer[read - RETAIN..read]);
        } else {
            let discard = (retained.len() + read).saturating_sub(RETAIN);
            retained.drain(..discard);
            retained.extend(&buffer[..read]);
        }
    }
    Ok(tail(&String::from_utf8_lossy(retained.make_contiguous())))
}

const TAIL_BYTES: usize = 4096;

#[cfg(any(not(unix), test))]
fn read_output_tail(mut reader: impl Read) -> io::Result<String> {
    // Keep a full codepoint of lookbehind. Decoding only the final 4096 raw
    // bytes could manufacture replacement characters at a cut UTF-8 boundary.
    // Lossy decoding never shrinks bytes; the extra three bytes keep that
    // possible boundary damage outside the final decoded tail.
    const RETAIN: usize = TAIL_BYTES + 3;
    let mut retained = VecDeque::with_capacity(RETAIN);
    let mut buffer = [0; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if read >= RETAIN {
            retained.clear();
            retained.extend(&buffer[read - RETAIN..read]);
        } else {
            let discard = (retained.len() + read).saturating_sub(RETAIN);
            retained.drain(..discard);
            retained.extend(&buffer[..read]);
        }
    }
    Ok(tail(&String::from_utf8_lossy(retained.make_contiguous())))
}

fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} in '{}' failed: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Last 4 KiB — enough to see the error, small enough to keep in a KV row.
fn tail(raw: &str) -> String {
    if raw.len() <= TAIL_BYTES {
        return raw.to_string();
    }
    let start = raw.len() - TAIL_BYTES;
    // Do not slice mid-codepoint.
    let start = (start..raw.len())
        .find(|i| raw.is_char_boundary(*i))
        .unwrap_or(raw.len());
    raw[start..].to_string()
}

fn dir_size(path: &Path) -> i64 {
    let mut total: i64 = 0;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else {
            total = total.saturating_add(meta.len() as i64);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Chunked<'a> {
        remaining: &'a [u8],
        chunk: usize,
        interrupted: bool,
    }

    impl Read for Chunked<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.interrupted {
                self.interrupted = false;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let len = self.remaining.len().min(self.chunk).min(output.len());
            output[..len].copy_from_slice(&self.remaining[..len]);
            self.remaining = &self.remaining[len..];
            Ok(len)
        }
    }

    #[test]
    fn build_command_withholds_credentials_but_keeps_the_toolchain() {
        let command = build_command(
            &BuildCommand {
                program: "cargo".into(),
                args: vec!["test".into()],
                features: Vec::new(),
            },
            Path::new("/checkout"),
            Path::new("/target"),
        );
        let removed: Vec<String> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().to_string())
            .collect();
        assert!(
            removed.iter().any(|name| name == "TACHI_DAEMON"),
            "{removed:?}"
        );
        assert!(
            removed
                .iter()
                .all(|name| name == "TACHI_DAEMON"
                    || memcore::child_env::is_credential_env_name(name)),
            "{removed:?}"
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == std::ffi::OsStr::new("CARGO_TARGET_DIR"))
                .map(|(_, value)| value),
            Some(Some(std::ffi::OsStr::new("/target")))
        );
    }

    #[test]
    fn bounded_tail_preserves_full_lossy_decode_at_utf8_and_read_boundaries() {
        let mut cut_codepoint = vec![b'A'; 5000];
        cut_codepoint.extend_from_slice("🚀".as_bytes());
        cut_codepoint.extend(vec![b'B'; 4093]);
        // A raw 4096-byte cap would invent a replacement character here.
        assert_ne!(
            tail(&String::from_utf8_lossy(&cut_codepoint)),
            tail(&String::from_utf8_lossy(
                &cut_codepoint[cut_codepoint.len() - TAIL_BYTES..]
            ))
        );

        let mut mixed =
            b"ASCII-\xe4\xb8\xad\xe6\x96\x87-\xf0\x9f\x9a\x80-\xff-\xe2\x82-\xc0\xaf".repeat(1500);
        mixed.extend_from_slice(b"\xf0\x9f");
        let bytes: Vec<u8> = (0..20000).map(|i| ((i * 73 + 19) % 256) as u8).collect();
        for input in [
            &cut_codepoint[..],
            &mixed,
            &bytes,
            &mixed[..4095],
            &mixed[..4096],
            &mixed[..4099],
        ] {
            let expected = tail(&String::from_utf8_lossy(input));
            for chunk in [1, 2, 3, 7, 4096, 8192] {
                let reader = Chunked {
                    remaining: input,
                    chunk,
                    interrupted: true,
                };
                assert_eq!(read_output_tail(reader).unwrap(), expected, "chunk={chunk}");
            }
        }
    }

    #[test]
    fn pipe_reader_reports_read_failure_instead_of_a_successful_empty_tail() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("injected capture failure"))
            }
        }
        let (input_tx, input_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::channel();
        input_tx.send(Broken).unwrap();
        drain_pipe(input_rx, result_tx, "stderr");
        let (stream, result) = result_rx.recv().unwrap();
        assert_eq!(stream, "stderr");
        assert_eq!(
            result.unwrap_err(),
            "read build stderr: injected capture failure"
        );
    }

    #[cfg(unix)]
    fn shell_ticket(args: Vec<String>) -> BuildTicket {
        BuildTicket::new(
            "real-runner-test",
            SourceIdentity {
                repo_root: "/runner-test".into(),
                base_sha: "a".repeat(40),
                head_sha: "a".repeat(40),
            },
            super::super::ticket::BuildCommand {
                program: "sh".into(),
                args,
                features: vec![],
            },
            None,
            None,
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn process_runner_drains_both_large_streams_and_preserves_exit_and_tails() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path().canonicalize().unwrap();
        let target = checkout.join("target");
        let script = r#"
            [ "$PWD" = "$1" ] && [ "$CARGO_TARGET_DIR" = "$2" ] || exit 93
            if IFS= read -r ignored; then exit 94; fi
            i=0
            while [ "$i" -lt 8192 ]; do
                printf 'stdout-0123456789'
                printf 'stderr-0123456789' >&2
                i=$((i + 1))
            done
            printf '\nOUT-中文-🚀-\377-\342\202'
            printf '\nERR-终点-🌍-\300\257-\360\237' >&2
            exit 7
        "#;
        let ticket = shell_ticket(vec![
            "-c".into(),
            script.into(),
            "runner-test".into(),
            checkout.to_string_lossy().into_owned(),
            target.to_string_lossy().into_owned(),
        ]);
        let run = ProcessBuildRunner.run(&ticket, &checkout, &target).unwrap();
        let mut stdout = b"stdout-0123456789".repeat(8192);
        stdout.extend_from_slice(b"\nOUT-\xe4\xb8\xad\xe6\x96\x87-\xf0\x9f\x9a\x80-\xff-\xe2\x82");
        let mut stderr = b"stderr-0123456789".repeat(8192);
        stderr.extend_from_slice(
            b"\nERR-\xe7\xbb\x88\xe7\x82\xb9-\xf0\x9f\x8c\x8d-\xc0\xaf-\xf0\x9f",
        );
        assert_eq!(run.outcome, BuildOutcome::Failed);
        assert_eq!(run.exit_code, Some(7));
        assert_eq!(run.stdout_tail, tail(&String::from_utf8_lossy(&stdout)));
        assert_eq!(run.stderr_tail, tail(&String::from_utf8_lossy(&stderr)));
    }

    #[cfg(unix)]
    #[test]
    fn process_runner_keeps_signal_exit_distinct_and_spawn_failure_returns() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path();
        let target = checkout.join("target");
        let mut ticket = shell_ticket(vec![
            "-c".into(),
            "printf interrupted; kill -TERM $$".into(),
        ]);
        let run = ProcessBuildRunner.run(&ticket, checkout, &target).unwrap();
        assert_eq!(run.outcome, BuildOutcome::Interrupted);
        assert_eq!(run.exit_code, None);
        assert_eq!(run.stdout_tail, "interrupted");
        ticket.command.program = checkout
            .join("absent-command")
            .to_string_lossy()
            .into_owned();
        let error = ProcessBuildRunner
            .run(&ticket, checkout, &target)
            .unwrap_err();
        assert!(error.starts_with("spawn "), "{error}");
    }

    #[cfg(unix)]
    fn read_test_pid(path: &Path) -> u32 {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
            .trim()
            .parse()
            .expect("numeric fixture pid")
    }

    #[cfg(unix)]
    fn assert_process_gone(pid: u32) {
        let deadline = Instant::now() + GROUP_REAP_TIMEOUT;
        while Instant::now() < deadline {
            if unix::process_absent(pid) {
                return;
            }
            std::thread::sleep(ROOT_POLL_INTERVAL);
        }
        // Even a failing fixture must not leak a five-minute sleep.
        unix::kill_process_for_test(pid);
        panic!("fixture process {pid} survived runner cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn root_exit_reaps_descendant_holding_both_flooded_pipes_before_build_run() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path();
        let target = checkout.join("target");
        let script = r#"
            sleep 300 & helper=$!
            printf '%s\n' "$$" > "$1/root.pid"
            printf '%s\n' "$helper" > "$1/helper.pid"
            i=0
            while [ "$i" -lt 8192 ]; do
                printf 'stdout-0123456789'
                printf 'stderr-0123456789' >&2
                i=$((i + 1))
            done
            printf 'ROOT-STDOUT-END'
            printf 'ROOT-STDERR-END' >&2
            exit 0
        "#;
        let ticket = shell_ticket(vec![
            "-c".into(),
            script.into(),
            "runner-test".into(),
            checkout.to_string_lossy().into_owned(),
        ]);

        let run = ProcessBuildRunner.run(&ticket, checkout, &target).unwrap();

        let expected_stdout = format!("{}ROOT-STDOUT-END", "stdout-0123456789".repeat(8192));
        let expected_stderr = format!("{}ROOT-STDERR-END", "stderr-0123456789".repeat(8192));
        assert_eq!(
            run,
            BuildRun {
                outcome: BuildOutcome::Success,
                exit_code: Some(0),
                stdout_tail: tail(&expected_stdout),
                stderr_tail: tail(&expected_stderr),
            },
            "the receipt input must preserve the root's status and both bounded tails"
        );
        assert_process_gone(read_test_pid(&checkout.join("root.pid")));
        assert_process_gone(read_test_pid(&checkout.join("helper.pid")));
    }

    #[cfg(unix)]
    #[test]
    fn reader_failure_uses_the_same_group_cleanup_and_reaps_root() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path();
        let target = checkout.join("target");
        let ready = checkout.join("ready");
        let script = r#"
            sleep 300 & helper=$!
            printf '%s\n' "$$" > "$1/root.pid"
            printf '%s\n' "$helper" > "$1/helper.pid"
            : > "$1/ready"
            while :; do sleep 60; done
        "#;
        let ticket = shell_ticket(vec![
            "-c".into(),
            script.into(),
            "runner-test".into(),
            checkout.to_string_lossy().into_owned(),
        ]);

        let error = run_process(
            &ticket,
            checkout,
            &target,
            Some(CaptureFault::ReadFailure {
                stream: "stdout",
                ready,
            }),
        )
        .unwrap_err();

        assert_eq!(error, "read build stdout: injected capture failure");
        assert_process_gone(read_test_pid(&checkout.join("root.pid")));
        assert_process_gone(read_test_pid(&checkout.join("helper.pid")));
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_reader_results_cleanup_a_live_root_and_descendant() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path();
        let target = checkout.join("target");
        let script = r#"
            sleep 300 >&- 2>&- & helper=$!
            printf '%s\n' "$$" > "$1/root.pid"
            printf '%s\n' "$helper" > "$1/helper.pid"
            exec 1>&- 2>&-
            wait
        "#;
        let ticket = shell_ticket(vec![
            "-c".into(),
            script.into(),
            "runner-test".into(),
            checkout.to_string_lossy().into_owned(),
        ]);

        let error = run_process(
            &ticket,
            checkout,
            &target,
            Some(CaptureFault::WithholdResults),
        )
        .unwrap_err();

        assert_eq!(error, "build output reader stopped without a result");
        assert_process_gone(read_test_pid(&checkout.join("root.pid")));
        assert_process_gone(read_test_pid(&checkout.join("helper.pid")));
    }

    #[cfg(unix)]
    #[test]
    fn escaped_descendant_fixture() {
        let Some(ready) = std::env::var_os("TACHI_BUILD_BROKER_SETSID_READY") else {
            return;
        };
        let session = unix::enter_new_session_for_test();
        // Always release the root, even if this platform refuses setsid. The
        // parent test will then observe a failed discrimination, never a hang.
        std::fs::write(ready, std::process::id().to_string()).unwrap();
        session.unwrap();
        std::thread::sleep(Duration::from_secs(300));
    }

    #[cfg(unix)]
    #[test]
    fn setsid_pipe_escape_returns_an_error_instead_of_holding_the_slot() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path();
        let target = checkout.join("target");
        let current_test = std::env::current_exe().unwrap();
        let script = r#"
            TACHI_BUILD_BROKER_SETSID_READY="$1/ready" \
                "$2" runner::tests::escaped_descendant_fixture --exact --nocapture &
            helper=$!
            printf '%s\n' "$$" > "$1/root.pid"
            printf '%s\n' "$helper" > "$1/helper.pid"
            while [ ! -f "$1/ready" ]; do sleep 0.01; done
            exit 0
        "#;
        let ticket = shell_ticket(vec![
            "-c".into(),
            script.into(),
            "runner-test".into(),
            checkout.to_string_lossy().into_owned(),
            current_test.to_string_lossy().into_owned(),
        ]);

        let result = ProcessBuildRunner.run(&ticket, checkout, &target);

        // The owned-group kill deliberately does not claim this escaped helper.
        // The test owns its explicit termination and waits for actual absence.
        let helper = read_test_pid(&checkout.join("helper.pid"));
        if !unix::process_absent(helper) {
            unix::kill_process_for_test(helper);
        }
        assert_process_gone(helper);
        assert_process_gone(read_test_pid(&checkout.join("root.pid")));
        let error = result.unwrap_err();
        assert!(
            error.contains("descendant outside") && error.contains("may still hold a pipe"),
            "escaped pipe retention must quarantine through a runner error: {error}"
        );
    }
}
