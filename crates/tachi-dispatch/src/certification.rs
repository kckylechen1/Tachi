//! Certification receipts + the vendor-version probe (#894 S2d, invariant 5).
//!
//! # Why a receipt exists at all
//!
//! The kill-test that certifies a provider ([`crate::authority`] invariant 5)
//! spawns a real vendor binary, needs real credentials, and takes ~60s. It
//! cannot run inside an ordinary `cargo test` — a CI box with no `codex` install
//! would either fail the suite or, far worse, "pass" it without exercising
//! anything. So the kill-test stays `#[ignore]`d and certification becomes what
//! it actually is: **an out-of-band event with a checked-in, machine-verified
//! receipt**.
//!
//! A [`CertificationReceipt`] is a statement about **one execution** of **one
//! kill-test** against **one vendor binary version** on **one OS**, covering
//! **one mutation matrix**. It is not a statement about the vendor, about
//! "sandboxes" in general, or about any other version of the same binary. Round
//! 2's ratchet said "a `KillTested` row may not point at an `#[ignore]`d test";
//! this replaces it with the stronger, truer one:
//!
//! > A row may claim `KillTested` **iff** a receipt exists that (a) passed, (b)
//! > names an existing kill-test, (c) covers the level being asked for, and (d)
//! > matches the version of the vendor binary that is actually installed.
//!
//! (d) is the part that has teeth at runtime. `codex --version` is probed before
//! spawn ([`probe_backend_version`]); a version the receipt does not cover —
//! including a version we could not determine at all — makes the row behave
//! exactly like [`crate::authority::Certification::Unverified`]: the read-only
//! lane fails closed. Provider conformance does not carry across vendor
//! versions, and an unknown version is not a certified version.
//!
//! # The artifact of record
//!
//! `crates/tachi-dispatch/certifications/*.toml` is the human-auditable receipt
//! (who ran it, when, on what binary, which mutations, which git blob of the
//! test). [`CODEX_CLI_RECEIPT`] below is its Rust mirror — the qualification
//! table points at the const, so certification is compiled into the binary and
//! cannot go missing on a deployed host. `receipt_const_matches_the_checked_in_receipt_file`
//! fails the build if the two ever disagree, field for field (the #873
//! single-source + embed + parity-test pattern).

use crate::authority::{version_components, TransportKind, WorkspaceAuthority};
use std::collections::HashMap;
#[cfg(unix)]
use std::io::Read;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
#[cfg(unix)]
use std::time::Instant;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
const VERSION_OUTPUT_MAX_BYTES: usize = 4 * 1024;
const CANONICAL_VERSION_MAX_BYTES: usize = 64;
#[cfg(unix)]
const PROBE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

/// The outcome of an executed kill-test. Only [`CertificationResult::Pass`]
/// certifies anything; a recorded `Fail` is kept deliberately expressible so a
/// regression can be *checked in* rather than silently deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificationResult {
    Pass,
    Fail,
}

impl CertificationResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

/// One executed kill-test, recorded. Every field is a fact about a run that
/// happened, not a plan for one that might.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificationReceipt {
    /// Stable id, stamped into dispatch receipts so a reader can find the file.
    pub id: &'static str,
    /// The checked-in artifact of record this const mirrors.
    pub source_file: &'static str,
    pub backend: &'static str,
    pub transport: TransportKind,
    /// What `--version` calls itself (e.g. `codex-cli`).
    pub vendor_binary: &'static str,
    /// The EXACT version that was exercised. The runtime gate compares the
    /// installed binary against this and refuses on any mismatch.
    pub vendor_version: &'static str,
    pub host_os: &'static str,
    pub host_os_version: &'static str,
    /// Repo-relative path of the kill-test, and the test fn inside it.
    pub kill_test: &'static str,
    pub kill_test_fn: &'static str,
    pub result: CertificationResult,
    pub executed_at: &'static str,
    pub executed_by: &'static str,
    pub duration_secs: &'static str,
    /// The commit whose kill-test source was executed, and that source's git
    /// blob — so "what exactly ran" is diffable, not a memory.
    pub executed_on_commit: &'static str,
    pub kill_test_source_blob: &'static str,
    /// The workspace-authority levels this execution certifies — and ONLY these.
    /// A level the run did not exercise is never certified by it.
    pub covers: &'static [WorkspaceAuthority],
    /// The mutation labels the sandbox was observed to refuse. Machine-coupled
    /// to the kill-test's own command table by
    /// `the_certified_matrix_matches_the_kill_tests_command_table`, so a matrix
    /// that grows without a re-run turns red instead of quietly over-claiming.
    pub matrix: &'static [&'static str],
}

impl CertificationReceipt {
    pub fn passed(self) -> bool {
        self.result == CertificationResult::Pass
    }

    /// Does this receipt attest the *installed* binary? `None` (we could not
    /// determine the version) is a NO — an unknown version is not a certified
    /// version (fail closed).
    pub fn certifies_version(self, installed: Option<&str>) -> bool {
        installed.is_some_and(|actual| versions_match(actual, self.vendor_version))
    }

    pub fn certifies_level(self, level: WorkspaceAuthority) -> bool {
        self.covers.contains(&level)
    }
}

/// codex CLI, read-only, macOS — the one certification that exists.
///
/// Mirrors `crates/tachi-dispatch/certifications/codex-cli.toml`. See that file
/// for the boundaries of the experiment; the short version is that this
/// certifies **codex-cli 0.144.1 on macOS at `read-only`, over the ten mutations
/// listed**, and nothing else.
pub const CODEX_CLI_RECEIPT: CertificationReceipt = CertificationReceipt {
    id: "codex-cli-0.144.1-macos-20260713",
    source_file: "crates/tachi-dispatch/certifications/codex-cli.toml",
    backend: "codex",
    transport: TransportKind::Cli,
    vendor_binary: "codex-cli",
    vendor_version: "0.144.1",
    host_os: "macos",
    host_os_version: "26.5.1",
    kill_test: "crates/tachi-dispatch/tests/codex_sandbox_kill_test.rs",
    kill_test_fn: "codex_read_only_sandbox_refuses_every_mutation_in_the_matrix",
    result: CertificationResult::Pass,
    executed_at: "2026-07-13",
    executed_by: "Oz",
    duration_secs: "60.71",
    executed_on_commit: "9b531012264db0b7bf0f50398ec7779b0bb13ffc",
    kill_test_source_blob: "c4df6aebfe9394f92f854c88f9c8f7a1b637b912",
    // Read-only ONLY: the run observed nothing about workspace-write containment.
    covers: &[WorkspaceAuthority::ReadOnly],
    matrix: CODEX_KILL_TEST_MATRIX,
};

/// The mutation labels [`CODEX_CLI_RECEIPT`] was issued for. Lives here (not in
/// the test) because it is part of the *claim*: the kill-test's command table is
/// checked against it on every ordinary `cargo test` run.
pub const CODEX_KILL_TEST_MATRIX: &[&str] = &[
    "create",
    "append",
    "truncate",
    "rename",
    "unlink",
    "chmod_then_write",
    "git_internals",
    "mounted_skill_write",
    "absolute_path_outside_worktree",
    "descendant_process_write",
];

/// Every receipt that ships. Used by the ratchet tests; the qualification table
/// references the consts directly.
pub const RECEIPTS: &[&CertificationReceipt] = &[&CODEX_CLI_RECEIPT];

// ─── Version comparison ──────────────────────────────────────────────────────

/// Do these two version strings name the same release? Compared numerically by
/// component (`0.144.1` == `v0.144.1`), never lexically — and an unparseable
/// version only matches itself, exactly.
pub fn versions_match(actual: &str, certified: &str) -> bool {
    match (version_components(actual), version_components(certified)) {
        (Some(actual), Some(certified)) => actual == certified,
        _ => actual.trim().eq_ignore_ascii_case(certified.trim()),
    }
}

/// Pull the version token out of a `--version` line and return only its bounded
/// numeric representation: `codex-cli v0.144.1+host-label` -> `0.144.1`.
/// Requires at least two dotted numeric components, so a stray `1`, a binary
/// name, or an unbounded version-shaped payload never enters a receipt.
pub fn parse_version_output(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|token| {
        let parts = version_components(token)?;
        if parts.len() < 2 {
            return None;
        }
        let canonical = parts
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".");
        (canonical.len() <= CANONICAL_VERSION_MAX_BYTES).then_some(canonical)
    })
}

// ─── The runtime version gate ────────────────────────────────────────────────

/// What the installed vendor binary reports, or `None` when we could not find
/// out (not on `PATH`, non-zero exit, unparseable output, or it hung).
///
/// `None` is honest ignorance and it **fails closed**: `qualify_provider` refuses
/// to hand back a certified row for a version it cannot confirm, so a read-only
/// dispatch is refused rather than run against an unknown binary that may not
/// enforce anything.
///
/// Called once per dispatch, on the pre-spawn path, and only for providers that
/// even have a sandbox primitive (there is nothing to gate otherwise). The result
/// is cached per binary *identity* — path + mtime + length — so upgrading codex
/// under a running daemon invalidates the cache instead of certifying the new
/// binary with the old binary's answer.
pub fn probe_backend_version(backend: &str) -> Option<String> {
    let program = which(backend)?;
    let identity = binary_identity(&program);

    let cache = version_cache();
    if let Ok(cache) = cache.lock() {
        if let Some((cached_identity, version)) = cache.get(backend) {
            if *cached_identity == identity {
                return version.clone();
            }
        }
    }

    let version = run_version_probe(&program);

    if let Ok(mut cache) = cache.lock() {
        cache.insert(backend.to_string(), (identity, version.clone()));
    }
    version
}

/// Closed prerequisite verdict for the one account-bearing backend probe.
/// Probe output never crosses this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendAccountProbe {
    Available,
    ExecutableUnavailable,
    AccountUnavailable,
    TimedOut,
    ContainmentUnavailable,
    CleanupUnconfirmed,
}

/// Check the existing Codex CLI account through the same bounded, contained
/// process owner used by the version prerequisite. No caller-supplied command,
/// environment, working directory, or credential enters this boundary.
pub fn probe_codex_account(timeout: Duration) -> BackendAccountProbe {
    let Some(program) = which("codex") else {
        return BackendAccountProbe::ExecutableUnavailable;
    };
    run_account_probe_with_timeout(&program, timeout)
}

type BinaryIdentity = (PathBuf, Option<SystemTime>, u64);

#[allow(clippy::type_complexity)]
fn version_cache() -> &'static Mutex<HashMap<String, (BinaryIdentity, Option<String>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (BinaryIdentity, Option<String>)>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn binary_identity(program: &std::path::Path) -> BinaryIdentity {
    let meta = std::fs::metadata(program).ok();
    (
        program.to_path_buf(),
        meta.as_ref().and_then(|m| m.modified().ok()),
        meta.map(|m| m.len()).unwrap_or(0),
    )
}

/// `<program> --version`, on a leash. The parent retains the child handle,
/// bounds both output streams, and joins both readers after confirmed cleanup.
/// If cleanup cannot be confirmed within its own deadline, an already-running
/// reaper thread retains every process and reader handle until cleanup really
/// completes; the caller fails closed without blocking indefinitely.
fn run_version_probe(program: &std::path::Path) -> Option<String> {
    run_version_probe_with_timeout(program, Duration::from_secs(5))
}

#[cfg(unix)]
fn read_version_probe_stream(mut stream: impl Read, deadline: Instant) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(VERSION_OUTPUT_MAX_BYTES);
    let mut chunk = [0_u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return Some(bytes),
            Ok(read) => {
                if bytes.len() + read > VERSION_OUTPUT_MAX_BYTES {
                    return None;
                }
                bytes.extend_from_slice(&chunk[..read]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                if Instant::now() >= deadline {
                    return None;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(unix)]
type VersionProbeReaderResult = Option<Vec<u8>>;
#[cfg(unix)]
type VersionProbeReaderJob = Box<dyn FnOnce() -> VersionProbeReaderResult + Send + 'static>;
#[cfg(unix)]
type VersionProbeReaderHandle = std::thread::JoinHandle<VersionProbeReaderResult>;
#[cfg(unix)]
type VersionProbeReaderSpawner<'a> = dyn FnMut(&'static str, VersionProbeReaderJob) -> std::io::Result<VersionProbeReaderHandle>
    + 'a;

#[cfg(unix)]
#[derive(Default)]
struct VersionProbeReaders {
    stdout: Option<VersionProbeReaderHandle>,
    stderr: Option<VersionProbeReaderHandle>,
}

#[cfg(unix)]
impl VersionProbeReaders {
    fn join_all(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        // Take and join both before inspecting either result. Missing, failed,
        // overflowed, or panicked stdout must never detach stderr (or vice
        // versa).
        let stdout = self.stdout.take().map(std::thread::JoinHandle::join);
        let stderr = self.stderr.take().map(std::thread::JoinHandle::join);
        match (stdout, stderr) {
            (Some(Ok(Some(stdout))), Some(Ok(Some(stderr)))) => Some((stdout, stderr)),
            _ => None,
        }
    }
}

#[cfg(unix)]
impl Drop for VersionProbeReaders {
    fn drop(&mut self) {
        let _ = self.join_all();
    }
}

#[cfg(unix)]
fn spawn_version_probe_reader(
    name: &'static str,
    job: VersionProbeReaderJob,
) -> std::io::Result<VersionProbeReaderHandle> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(job)
}

#[cfg(unix)]
fn set_nonblocking(stream: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    let fd = stream.as_raw_fd();
    // SAFETY: fcntl receives the live pipe descriptor and scalar flags only.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the same owned pipe remains live for this call; preserving the
    // existing flags and adding O_NONBLOCK makes reader deadlines enforceable.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn observe_probe_root_exit_without_reap(pid: u32) -> std::io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: waitid writes one siginfo_t and WNOWAIT deliberately retains the
    // group leader so its numeric PGID cannot be reused before group cleanup.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(unix)]
fn signal_probe_process_group(pid: u32, signal: libc::c_int) -> bool {
    // SAFETY: the child was spawned with process_group(0), so its pid is the
    // owned PGID. A negative pid addresses only that group.
    if unsafe { libc::kill(-(pid as libc::pid_t), signal) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
fn probe_process_group_absent(pid: u32) -> bool {
    // SAFETY: signal 0 is a non-mutating liveness probe for the owned PGID.
    let result = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
    result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct VersionProbeCleanupOps {
    signal: fn(u32, libc::c_int) -> bool,
    group_absent: fn(u32) -> bool,
    try_wait: fn(&mut std::process::Child) -> std::io::Result<Option<std::process::ExitStatus>>,
}

#[cfg(unix)]
fn try_wait_probe_root(
    child: &mut std::process::Child,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    child.try_wait()
}

#[cfg(unix)]
const VERSION_PROBE_CLEANUP_OPS: VersionProbeCleanupOps = VersionProbeCleanupOps {
    signal: signal_probe_process_group,
    group_absent: probe_process_group_absent,
    try_wait: try_wait_probe_root,
};

#[cfg(unix)]
struct OwnedVersionProbe {
    child: std::process::Child,
    pid: u32,
    root_status: Option<std::process::ExitStatus>,
    readers: VersionProbeReaders,
}

#[cfg(unix)]
impl OwnedVersionProbe {
    fn new(child: std::process::Child) -> Self {
        let pid = child.id();
        Self {
            child,
            pid,
            root_status: None,
            readers: VersionProbeReaders::default(),
        }
    }

    fn spawn_readers(
        &mut self,
        stdout: std::process::ChildStdout,
        stderr: std::process::ChildStderr,
        deadline: Instant,
        spawn: &mut VersionProbeReaderSpawner<'_>,
    ) -> std::io::Result<()> {
        self.readers.stdout = Some(spawn(
            "tachi-version-stdout",
            Box::new(move || read_version_probe_stream(stdout, deadline)),
        )?);
        self.readers.stderr = Some(spawn(
            "tachi-version-stderr",
            Box::new(move || read_version_probe_stream(stderr, deadline)),
        )?);
        Ok(())
    }

    fn join_readers(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        self.readers.join_all()
    }

    fn terminate_reap_and_prove(
        &mut self,
        deadline: Instant,
        ops: VersionProbeCleanupOps,
    ) -> Option<std::process::ExitStatus> {
        // Signal while the unreaped leader still reserves this numeric PGID.
        // A zombie leader itself keeps kill(-pgid, 0) successful on macOS;
        // requiring group absence before reaping would never finish.
        if self.root_status.is_none() {
            // A naturally exited zombie-only group may reject SIGKILL with
            // EPERM on macOS. Neither success nor failure proves termination;
            // the subsequent reap plus ESRCH observation is authoritative.
            let _ = (ops.signal)(self.pid, libc::SIGKILL);
        }
        loop {
            if self.root_status.is_none() {
                if let Ok(Some(status)) = (ops.try_wait)(&mut self.child) {
                    self.root_status = Some(status);
                }
            }
            if let Some(status) = self.root_status {
                // After reap, only observe. Never signal a potentially reused PGID.
                if (ops.group_absent)(self.pid) {
                    return Some(status);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn cleanup_until_confirmed(mut self) {
        loop {
            if self.root_status.is_none() {
                let _ = signal_probe_process_group(self.pid, libc::SIGKILL);
                if let Ok(Some(status)) = self.child.try_wait() {
                    self.root_status = Some(status);
                }
            }
            if self.root_status.is_some() && probe_process_group_absent(self.pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Process-tree cleanup comes first so inherited pipes close before all
        // successfully-created readers are joined.
        let _ = self.readers.join_all();
    }
}

#[cfg(unix)]
struct VersionProbeCleanupOwner {
    sender: Option<mpsc::SyncSender<OwnedVersionProbe>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl VersionProbeCleanupOwner {
    fn start() -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<OwnedVersionProbe>(1);
        let thread = std::thread::Builder::new()
            .name("tachi-prerequisite-reaper".to_string())
            .spawn(move || {
                if let Ok(probe) = receiver.recv() {
                    probe.cleanup_until_confirmed();
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    fn finish(mut self) {
        drop(self.sender.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn retain_until_confirmed(mut self, probe: OwnedVersionProbe) {
        let sender = self
            .sender
            .take()
            .expect("prerequisite cleanup sender must exist");
        // The receiver performs no fallible setup and cannot exit before this
        // send. Its pre-spawn creation is the refusal boundary that guarantees
        // a live child always has an owner even when bounded cleanup expires.
        if let Err(failed) = sender.send(probe) {
            // The receiver has no fallible work before recv, so this branch is
            // unreachable in ordinary operation. If the owner thread itself
            // was externally lost, retain ownership here rather than dropping
            // a live child handle.
            failed.0.cleanup_until_confirmed();
        }
        // Deliberately detach: the reaper now owns the child and readers and
        // may outlive this failed prerequisite without blocking its caller.
        drop(self.thread.take());
    }
}

#[cfg(unix)]
struct VersionProbeGuard {
    probe: Option<OwnedVersionProbe>,
    cleanup: Option<VersionProbeCleanupOwner>,
}

#[cfg(unix)]
impl VersionProbeGuard {
    fn new(child: std::process::Child, cleanup: VersionProbeCleanupOwner) -> Self {
        Self {
            probe: Some(OwnedVersionProbe::new(child)),
            cleanup: Some(cleanup),
        }
    }

    fn probe_mut(&mut self) -> &mut OwnedVersionProbe {
        self.probe.as_mut().expect("owned prerequisite probe")
    }

    fn into_confirmed(mut self) -> OwnedVersionProbe {
        let probe = self.probe.take().expect("confirmed prerequisite probe");
        self.cleanup
            .take()
            .expect("prerequisite cleanup owner")
            .finish();
        probe
    }
}

#[cfg(unix)]
impl Drop for VersionProbeGuard {
    fn drop(&mut self) {
        if let Some(probe) = self.probe.take() {
            self.cleanup
                .take()
                .expect("live prerequisite probe must retain cleanup owner")
                .retain_until_confirmed(probe);
        } else if let Some(cleanup) = self.cleanup.take() {
            cleanup.finish();
        }
    }
}

#[cfg(unix)]
fn run_version_probe_with_timeout(program: &std::path::Path, timeout: Duration) -> Option<String> {
    let mut spawn_reader = spawn_version_probe_reader;
    run_version_probe_with_timeout_and_spawner_and_cleanup(
        program,
        timeout,
        PROBE_CLEANUP_TIMEOUT,
        VERSION_PROBE_CLEANUP_OPS,
        &mut spawn_reader,
        crate::configure_process_group_escape_containment,
    )
}

#[cfg(all(unix, test))]
fn fixture_containment(command: &mut std::process::Command) -> bool {
    #[cfg(target_os = "macos")]
    {
        let _ = command;
        // Only the known test scripts use this route. They do not call setsid;
        // these tests check cleanup mechanics, not provider certification.
        true
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::configure_process_group_escape_containment(command)
    }
}

#[cfg(all(unix, test))]
fn run_fixture_version_probe_with_timeout(
    program: &std::path::Path,
    timeout: Duration,
) -> Option<String> {
    let mut spawn_reader = spawn_version_probe_reader;
    run_version_probe_with_timeout_and_spawner_and_cleanup(
        program,
        timeout,
        PROBE_CLEANUP_TIMEOUT,
        VERSION_PROBE_CLEANUP_OPS,
        &mut spawn_reader,
        fixture_containment,
    )
}

#[cfg(all(unix, test))]
fn run_version_probe_with_timeout_and_spawner(
    program: &std::path::Path,
    timeout: Duration,
    spawn_reader: &mut VersionProbeReaderSpawner<'_>,
) -> Option<String> {
    run_version_probe_with_timeout_and_spawner_and_cleanup(
        program,
        timeout,
        PROBE_CLEANUP_TIMEOUT,
        VERSION_PROBE_CLEANUP_OPS,
        spawn_reader,
        fixture_containment,
    )
}

#[cfg(unix)]
fn run_version_probe_with_timeout_and_spawner_and_cleanup(
    program: &std::path::Path,
    timeout: Duration,
    cleanup_timeout: Duration,
    cleanup_ops: VersionProbeCleanupOps,
    spawn_reader: &mut VersionProbeReaderSpawner<'_>,
    configure_containment: fn(&mut std::process::Command) -> bool,
) -> Option<String> {
    use std::os::unix::process::CommandExt;

    let deadline = Instant::now() + timeout;
    let mut command = std::process::Command::new(program);
    command.arg("--version");
    if !configure_containment(&mut command) {
        return None;
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let cleanup = VersionProbeCleanupOwner::start().ok()?;
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            cleanup.finish();
            return None;
        }
    };
    let mut guard = VersionProbeGuard::new(child, cleanup);
    let stdout = guard.probe_mut().child.stdout.take()?;
    let stderr = guard.probe_mut().child.stderr.take()?;
    set_nonblocking(&stdout).ok()?;
    set_nonblocking(&stderr).ok()?;
    guard
        .probe_mut()
        .spawn_readers(stdout, stderr, deadline, spawn_reader)
        .ok()?;

    let root_exited = loop {
        match observe_probe_root_exit_without_reap(guard.probe_mut().pid) {
            Ok(true) => break true,
            Ok(false) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(false) | Err(_) => break false,
        }
    };
    let status = guard
        .probe_mut()
        .terminate_reap_and_prove(Instant::now() + cleanup_timeout, cleanup_ops);
    status?;
    let mut owned = guard.into_confirmed();
    let (stdout, stderr) = owned.join_readers()?;

    if !root_exited || !status.expect("confirmed status").success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&stdout);
    parse_version_output(&stdout)
        .or_else(|| parse_version_output(&String::from_utf8_lossy(&stderr)))
}

#[cfg(unix)]
fn run_account_probe_with_timeout(
    program: &std::path::Path,
    timeout: Duration,
) -> BackendAccountProbe {
    run_account_probe_with_timeout_and_containment(
        program,
        timeout,
        crate::configure_process_group_escape_containment,
    )
}

#[cfg(unix)]
fn run_account_probe_with_timeout_and_containment(
    program: &std::path::Path,
    timeout: Duration,
    configure_containment: fn(&mut std::process::Command) -> bool,
) -> BackendAccountProbe {
    use std::os::unix::process::CommandExt;

    let deadline = Instant::now() + timeout;
    let mut command = std::process::Command::new(program);
    command.args(["login", "status"]);
    if !configure_containment(&mut command) {
        return BackendAccountProbe::ContainmentUnavailable;
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let cleanup = match VersionProbeCleanupOwner::start() {
        Ok(cleanup) => cleanup,
        Err(_) => return BackendAccountProbe::CleanupUnconfirmed,
    };
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            cleanup.finish();
            return BackendAccountProbe::ExecutableUnavailable;
        }
    };
    let mut guard = VersionProbeGuard::new(child, cleanup);
    let root_exited = loop {
        match observe_probe_root_exit_without_reap(guard.probe_mut().pid) {
            Ok(true) => break true,
            Ok(false) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(false) | Err(_) => break false,
        }
    };
    let status = guard.probe_mut().terminate_reap_and_prove(
        Instant::now() + PROBE_CLEANUP_TIMEOUT,
        VERSION_PROBE_CLEANUP_OPS,
    );
    let Some(status) = status else {
        return BackendAccountProbe::CleanupUnconfirmed;
    };
    let _owned = guard.into_confirmed();
    if !root_exited {
        BackendAccountProbe::TimedOut
    } else if !status.success() {
        BackendAccountProbe::AccountUnavailable
    } else {
        BackendAccountProbe::Available
    }
}

#[cfg(not(unix))]
fn run_version_probe_with_timeout(
    _program: &std::path::Path,
    _timeout: Duration,
) -> Option<String> {
    // The managed canary refuses before spawn on hosts where this crate cannot
    // own and prove termination of the prerequisite process tree.
    None
}

#[cfg(not(unix))]
fn run_account_probe_with_timeout(
    _program: &std::path::Path,
    _timeout: Duration,
) -> BackendAccountProbe {
    BackendAccountProbe::ContainmentUnavailable
}

/// First `program` on `PATH` — the same resolution `Command::new("codex")` does,
/// so we probe the binary that will actually be spawned. (A shell *alias* is not
/// on this path: `Command` never goes through a shell, which is why the kill-test
/// exercised the real binary and not the operator's
/// `codex --dangerously-bypass-approvals-and-sandbox` alias.)
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct VersionProbeFixture {
        root: PathBuf,
        program: PathBuf,
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl VersionProbeFixture {
        fn new(script: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            use std::sync::atomic::{AtomicU64, Ordering};

            static FIXTURE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
            // Several fixtures spin while testing deadlines and cleanup. Keep
            // those probes separate so one fixture cannot consume another's
            // one-second observation window under the parallel test harness.
            let serial = FIXTURE_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let root = std::env::temp_dir().join(format!(
                "tachi-version-probe-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).expect("create version probe fixture");
            let program = root.join("probe");
            std::fs::write(&program, script).expect("write version probe fixture");
            let mut permissions = std::fs::metadata(&program)
                .expect("version probe fixture metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&program, permissions)
                .expect("make version probe fixture executable");
            Self {
                root,
                program,
                _serial: serial,
            }
        }

        fn pid_path(&self) -> PathBuf {
            self.root.join("probe.pid")
        }

        fn descendant_pid_path(&self) -> PathBuf {
            self.root.join("probe.descendant.pid")
        }

        fn escaped_pid_path(&self) -> PathBuf {
            self.root.join("probe.escaped.pid")
        }

        #[cfg(target_os = "linux")]
        fn escape_status_path(&self) -> PathBuf {
            self.root.join("probe.escape.status")
        }
    }

    #[cfg(unix)]
    impl Drop for VersionProbeFixture {
        fn drop(&mut self) {
            // A deliberately broken pre-fix escape discriminator may leave a
            // setsid-created fixture group behind. Keep the test itself from
            // leaking that mutant when its assertion fires.
            if let Ok(raw) = std::fs::read_to_string(self.escaped_pid_path()) {
                if let Ok(pid) = raw.trim().parse::<libc::pid_t>() {
                    // SAFETY: this fixture records the leader of its own
                    // setsid-created group; failure means it is already gone.
                    let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
                    let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The checked-in receipt file, read at compile time.
    const RECEIPT_FILE: &str = include_str!("../certifications/codex-cli.toml");

    #[derive(Debug, PartialEq, Eq)]
    enum TomlValue {
        Str(String),
        List(Vec<String>),
    }

    impl TomlValue {
        fn as_str(&self) -> &str {
            match self {
                Self::Str(value) => value,
                Self::List(_) => panic!("expected a string, got a list"),
            }
        }

        fn as_list(&self) -> &[String] {
            match self {
                Self::List(values) => values,
                Self::Str(_) => panic!("expected a list, got a string"),
            }
        }
    }

    /// Deliberately tiny: the receipt format is `key = "value"` and
    /// `key = ["a", "b"]`, comments on their own lines. A real TOML dependency
    /// for one 20-key file would be the tail wagging the dog.
    fn parse_receipt(src: &str) -> HashMap<String, TomlValue> {
        let mut out = HashMap::new();
        let mut lines = src
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'));
        while let Some(line) = lines.next() {
            let Some((key, rest)) = line.split_once('=') else {
                continue;
            };
            let mut rest = rest.trim().to_string();
            if rest.starts_with('[') && !rest.contains(']') {
                for next in lines.by_ref() {
                    rest.push(' ');
                    rest.push_str(next);
                    if next.contains(']') {
                        break;
                    }
                }
            }
            let value = if let Some(inner) = rest.strip_prefix('[') {
                let inner = inner.trim_end_matches(']');
                TomlValue::List(
                    inner
                        .split(',')
                        .map(|item| item.trim().trim_matches('"').to_string())
                        .filter(|item| !item.is_empty())
                        .collect(),
                )
            } else {
                TomlValue::Str(rest.trim_matches('"').to_string())
            };
            out.insert(key.trim().to_string(), value);
        }
        out
    }

    /// **The parity ratchet.** The TOML file is the artifact a human audits; the
    /// const is what the qualification table actually trusts. If they can drift,
    /// the audit is theater — so they cannot drift.
    #[test]
    fn receipt_const_matches_the_checked_in_receipt_file() {
        let file = parse_receipt(RECEIPT_FILE);
        let receipt = CODEX_CLI_RECEIPT;

        for (key, expected) in [
            ("id", receipt.id),
            ("backend", receipt.backend),
            ("transport", "cli"),
            ("vendor_binary", receipt.vendor_binary),
            ("vendor_version", receipt.vendor_version),
            ("host_os", receipt.host_os),
            ("host_os_version", receipt.host_os_version),
            ("kill_test", receipt.kill_test),
            ("kill_test_fn", receipt.kill_test_fn),
            ("result", receipt.result.as_str()),
            ("executed_at", receipt.executed_at),
            ("executed_by", receipt.executed_by),
            ("duration_secs", receipt.duration_secs),
            ("executed_on_commit", receipt.executed_on_commit),
            ("kill_test_source_blob", receipt.kill_test_source_blob),
        ] {
            let got = file
                .get(key)
                .unwrap_or_else(|| panic!("receipt file is missing '{key}'"))
                .as_str();
            assert_eq!(
                got, expected,
                "'{key}' disagrees between {} and the Rust const",
                receipt.source_file
            );
        }

        let covers = file.get("covers").expect("covers").as_list();
        let const_covers = receipt
            .covers
            .iter()
            .map(|level| level.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(covers, const_covers.as_slice(), "certified levels disagree");

        let matrix = file.get("matrix").expect("matrix").as_list();
        assert_eq!(
            matrix, receipt.matrix,
            "the certified mutation matrix disagrees"
        );
        assert_eq!(
            receipt.transport,
            TransportKind::Cli,
            "the receipt file says transport = 'cli'"
        );
    }

    /// A receipt that did not pass, or that certifies nothing, must never reach
    /// the table. (Both are expressible on purpose — a recorded regression is
    /// worth keeping — but neither may certify.)
    #[test]
    fn shipped_receipts_passed_and_certify_at_least_one_level() {
        for receipt in RECEIPTS {
            assert!(receipt.passed(), "'{}' did not pass", receipt.id);
            assert!(
                !receipt.covers.is_empty(),
                "'{}' certifies no level",
                receipt.id
            );
            assert!(
                !receipt.matrix.is_empty(),
                "'{}' has an empty mutation matrix — it observed nothing",
                receipt.id
            );
        }
    }

    /// The codex run exercised read-only and *only* read-only. Pinned so a later
    /// edit cannot quietly widen `covers` to workspace-write, which no execution
    /// backs.
    #[test]
    fn the_codex_receipt_certifies_read_only_only() {
        assert_eq!(CODEX_CLI_RECEIPT.covers, &[WorkspaceAuthority::ReadOnly]);
        assert!(!CODEX_CLI_RECEIPT.certifies_level(WorkspaceAuthority::WorkspaceWrite));
        assert!(!CODEX_CLI_RECEIPT.certifies_level(WorkspaceAuthority::DangerFullAccess));
    }

    /// The runtime gate, in miniature: only the exercised version is certified,
    /// and an unknown version is not an old-enough version.
    #[test]
    fn a_receipt_certifies_exactly_the_version_it_exercised() {
        let receipt = CODEX_CLI_RECEIPT;
        assert!(receipt.certifies_version(Some("0.144.1")));
        assert!(
            receipt.certifies_version(Some("v0.144.1")),
            "the comparison is numeric-by-component, not a string match"
        );
        for other in ["0.144.2", "0.145.0", "0.144", "1.0.0", "0.9.0"] {
            assert!(
                !receipt.certifies_version(Some(other)),
                "'{other}' was never exercised and must not be certified"
            );
        }
        assert!(
            !receipt.certifies_version(None),
            "an unknown version is not a certified version (fail closed)"
        );
    }

    #[test]
    fn version_output_parsing_finds_the_version_token() {
        assert_eq!(
            parse_version_output("codex-cli 0.144.1").as_deref(),
            Some("0.144.1")
        );
        assert_eq!(
            parse_version_output("codex-cli v0.144.1\n").as_deref(),
            Some("0.144.1")
        );
        assert_eq!(
            parse_version_output("codex-cli 0.144.1+fixture-account-secret").as_deref(),
            Some("0.144.1"),
            "only bounded numeric components may leave the version probe"
        );
        // A bare integer is not a version; a name is not a version.
        assert_eq!(parse_version_output("codex 1"), None);
        assert_eq!(parse_version_output("no version here"), None);
        let unbounded = format!("codex {}", vec!["1"; 40].join("."));
        assert_eq!(
            parse_version_output(&unbounded),
            None,
            "unbounded component lists are not receipt-safe versions"
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_canonicalizes_hostile_stdout_and_stderr() {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf 'fixture-stdout-secret\\n'\nprintf 'codex-cli 0.144.1+fixture-stderr-secret\\n' >&2\n",
        );
        assert_eq!(
            run_fixture_version_probe_with_timeout(&fixture.program, Duration::from_secs(1))
                .as_deref(),
            Some("0.144.1"),
            "raw stdout/stderr labels must not survive the probe boundary"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_version_probe_refuses_before_spawn_without_containment() {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nprintf 'codex-cli 0.144.1\\n'\n",
        );
        assert_eq!(
            run_version_probe_with_timeout(&fixture.program, Duration::from_secs(1)),
            None,
        );
        assert!(
            !fixture.pid_path().exists(),
            "uncontained version probe spawned"
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_stdin_is_isolated_from_readable_parent_fd_zero() {
        const CHILD_ENV: &str = "TACHI_VERSION_PROBE_STDIN_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            let fixture = VersionProbeFixture::new(
                "#!/bin/sh\nif IFS= read -r daemon_input; then exit 91; fi\nprintf 'codex-cli 0.144.1\\n'\n",
            );
            assert_eq!(
                run_fixture_version_probe_with_timeout(&fixture.program, Duration::from_secs(1))
                    .as_deref(),
                Some("0.144.1"),
                "the version probe must observe EOF instead of readable parent stdin"
            );
            return;
        }

        use std::io::Write;
        let mut child = std::process::Command::new(
            std::env::current_exe().expect("resolve current test binary"),
        )
        .arg("version_probe_stdin_is_isolated_from_readable_parent_fd_zero")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn readable-stdin test child");
        child
            .stdin
            .take()
            .expect("test child stdin")
            .write_all(b"hostile-daemon-protocol-input\n")
            .expect("supply readable parent fd zero");
        assert!(
            child
                .wait()
                .expect("wait for readable-stdin test child")
                .success(),
            "the nested discriminator failed with readable fd zero"
        );
    }

    #[cfg(unix)]
    #[test]
    fn account_probe_stdin_is_isolated_from_readable_parent_fd_zero() {
        const CHILD_ENV: &str = "TACHI_ACCOUNT_PROBE_STDIN_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            let fixture = VersionProbeFixture::new(
                "#!/bin/sh\nif IFS= read -r daemon_input; then exit 91; fi\nexit 0\n",
            );
            let codex = fixture.root.join("codex");
            std::fs::hard_link(&fixture.program, &codex).expect("install account probe fixture");
            std::env::set_var("PATH", &fixture.root);
            assert_eq!(
                run_account_probe_with_timeout_and_containment(
                    &codex,
                    Duration::from_secs(1),
                    fixture_containment,
                ),
                BackendAccountProbe::Available,
                "the account probe must observe EOF instead of readable parent stdin"
            );
            #[cfg(target_os = "macos")]
            assert_eq!(
                probe_codex_account(Duration::from_secs(1)),
                BackendAccountProbe::ContainmentUnavailable,
                "the production probe must refuse before spawn without containment"
            );
            return;
        }

        use std::io::Write;
        let mut child = std::process::Command::new(
            std::env::current_exe().expect("resolve current test binary"),
        )
        .arg("account_probe_stdin_is_isolated_from_readable_parent_fd_zero")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn readable-stdin account test child");
        child
            .stdin
            .take()
            .expect("account test child stdin")
            .write_all(b"hostile-daemon-protocol-input\n")
            .expect("supply readable parent fd zero");
        assert!(
            child.wait().expect("wait for account test child").success(),
            "the account discriminator failed with readable fd zero"
        );
    }

    #[cfg(unix)]
    fn fixture_pid(path: &std::path::Path) -> libc::pid_t {
        std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("fixture did not publish {}: {error}", path.display()))
            .trim()
            .parse()
            .unwrap_or_else(|error| {
                panic!("fixture PID in {} was invalid: {error}", path.display())
            })
    }

    #[cfg(unix)]
    fn assert_pid_absent(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: signal 0 is a non-mutating probe for the fixture PID.
            if unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return;
            }
            if Instant::now() >= deadline {
                panic!("fixture PID {pid} remained present after cleanup handoff");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn cleanup_signal_failure(_pid: u32, _signal: libc::c_int) -> bool {
        false
    }

    #[cfg(unix)]
    fn cleanup_group_absence_failure(_pid: u32) -> bool {
        false
    }

    #[cfg(unix)]
    fn cleanup_wait_stall(
        _child: &mut std::process::Child,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        Ok(None)
    }

    #[cfg(unix)]
    fn cleanup_wait_error(
        _child: &mut std::process::Child,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        Err(std::io::Error::other("injected root wait error"))
    }

    #[cfg(unix)]
    fn assert_cleanup_fault_is_bounded(ops: VersionProbeCleanupOps) {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nwhile :; do :; done\n",
        );
        let started = Instant::now();
        let mut spawn_reader = spawn_version_probe_reader;
        assert_eq!(
            run_version_probe_with_timeout_and_spawner_and_cleanup(
                &fixture.program,
                Duration::from_millis(500),
                Duration::from_millis(100),
                ops,
                &mut spawn_reader,
                fixture_containment,
            ),
            None
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "faulted prerequisite cleanup exceeded its total caller wall-clock bound"
        );
        wait_for_fixture_file(&fixture.pid_path());
        let pid = fixture_pid(&fixture.pid_path());
        // Returning above is allowed only because the pre-created reaper owns
        // the live child and eventually establishes authoritative ESRCH.
        assert_pid_absent(pid);
        assert_process_group_absent(pid);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_of_reaped_root_never_signals_its_numeric_group_again() {
        use std::os::unix::process::CommandExt;
        fn forbidden_signal(_: u32, _: libc::c_int) -> bool {
            panic!("a reaped numeric group must never be signalled");
        }
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .expect("owned child");
        let mut owned = OwnedVersionProbe::new(child);
        owned.root_status = Some(owned.child.wait().expect("reap fixture"));
        assert!(owned
            .terminate_reap_and_prove(
                Instant::now() + Duration::from_secs(1),
                VersionProbeCleanupOps {
                    signal: forbidden_signal,
                    ..VERSION_PROBE_CLEANUP_OPS
                }
            )
            .expect("reaped root plus absent group")
            .success());
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_signal_failure_transfers_cleanup_ownership_within_deadline() {
        assert_cleanup_fault_is_bounded(VersionProbeCleanupOps {
            signal: cleanup_signal_failure,
            ..VERSION_PROBE_CLEANUP_OPS
        });
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_root_wait_stall_transfers_cleanup_ownership_within_deadline() {
        assert_cleanup_fault_is_bounded(VersionProbeCleanupOps {
            try_wait: cleanup_wait_stall,
            ..VERSION_PROBE_CLEANUP_OPS
        });
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_root_wait_error_transfers_cleanup_ownership_within_deadline() {
        assert_cleanup_fault_is_bounded(VersionProbeCleanupOps {
            try_wait: cleanup_wait_error,
            ..VERSION_PROBE_CLEANUP_OPS
        });
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_group_absence_failure_transfers_ownership_within_deadline() {
        assert_cleanup_fault_is_bounded(VersionProbeCleanupOps {
            group_absent: cleanup_group_absence_failure,
            ..VERSION_PROBE_CLEANUP_OPS
        });
    }

    #[cfg(unix)]
    fn assert_process_group_absent(pgid: libc::pid_t) {
        // SAFETY: the negative fixture root PID names only its owned group.
        assert_eq!(unsafe { libc::kill(-pgid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "fixture process group {pgid} survived probe return"
        );
    }

    #[cfg(unix)]
    fn wait_for_fixture_file(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !path.is_file() {
            assert!(
                Instant::now() < deadline,
                "fixture did not publish {} before the deadline",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_timeout_kills_reaps_and_proves_the_owned_group_absent() {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nwhile :; do :; done\n",
        );
        assert_eq!(
            run_fixture_version_probe_with_timeout(&fixture.program, Duration::from_millis(500)),
            None,
            "a hanging version probe must fail closed"
        );
        let pid = fixture_pid(&fixture.pid_path());
        assert_pid_absent(pid);
        assert_process_group_absent(pid);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_reaps_descendant_that_inherits_pipes_after_root_exit() {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\n/bin/sh -c 'printf \"%s\\n\" \"$$\" > \"$1\"; while :; do :; done' sh \"$0.descendant.pid\" &\nwhile [ ! -s \"$0.descendant.pid\" ]; do :; done\nprintf 'codex-cli 0.144.1\\n'\nexit 0\n",
        );
        let started = Instant::now();
        assert_eq!(
            run_fixture_version_probe_with_timeout(&fixture.program, Duration::from_secs(1))
                .as_deref(),
            Some("0.144.1"),
            "a root exit must close inherited pipes by terminating the owned group"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "inherited pipes must not move reader joins outside the deadline"
        );
        let root_pid = fixture_pid(&fixture.pid_path());
        let descendant_pid = fixture_pid(&fixture.descendant_pid_path());
        assert_pid_absent(root_pid);
        assert_pid_absent(descendant_pid);
        assert_process_group_absent(root_pid);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn version_probe_denies_setsid_escape_that_retains_inherited_pipes() {
        assert!(
            std::path::Path::new("/usr/bin/setsid").is_file(),
            "setsid escape discriminator requires /usr/bin/setsid"
        );
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\n/usr/bin/setsid /bin/sh -c 'printf \"%s\\n\" \"$$\" > \"$1\"; while :; do :; done' sh \"$0.escaped.pid\" &\nescape=$!\nwait \"$escape\"\nprintf '%s\\n' \"$?\" > \"$0.escape.status\"\nprintf 'codex-cli 0.144.1\\n'\n",
        );
        assert_eq!(
            run_version_probe_with_timeout(&fixture.program, Duration::from_millis(500)).as_deref(),
            Some("0.144.1"),
            "setsid must be denied rather than escaping with inherited pipes"
        );
        let escape_status = std::fs::read_to_string(fixture.escape_status_path())
            .expect("fixture must record the denied setsid status");
        assert_ne!(escape_status.trim(), "0", "setsid unexpectedly succeeded");
        assert!(
            !fixture.escaped_pid_path().exists(),
            "an escaped inherited-pipe descendant was created"
        );
        let root_pid = fixture_pid(&fixture.pid_path());
        assert_pid_absent(root_pid);
        assert_process_group_absent(root_pid);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn version_probe_denies_setsid_escape_that_closes_inherited_pipes() {
        assert!(
            std::path::Path::new("/usr/bin/setsid").is_file(),
            "setsid escape discriminator requires /usr/bin/setsid"
        );
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\n/usr/bin/setsid /bin/sh -c 'printf \"%s\\n\" \"$$\" > \"$1\"; exec </dev/null >/dev/null 2>/dev/null; while :; do :; done' sh \"$0.escaped.pid\" &\nescape=$!\ni=0\nwhile kill -0 \"$escape\" 2>/dev/null && [ ! -s \"$0.escaped.pid\" ] && [ \"$i\" -lt 100 ]; do /bin/sleep 0.01; i=$((i + 1)); done\nif ! kill -0 \"$escape\" 2>/dev/null; then wait \"$escape\"; printf '%s\\n' \"$?\" > \"$0.escape.status\"; fi\nprintf 'codex-cli 0.144.1\\n'\n",
        );
        assert_eq!(
            run_version_probe_with_timeout(&fixture.program, Duration::from_secs(2)).as_deref(),
            Some("0.144.1"),
            "a valid version must not hide a closed-pipe escaped descendant"
        );
        let escape_status = std::fs::read_to_string(fixture.escape_status_path())
            .expect("fixture must record the denied setsid status");
        assert_ne!(escape_status.trim(), "0", "setsid unexpectedly succeeded");
        assert!(
            !fixture.escaped_pid_path().exists(),
            "an escaped closed-pipe descendant was created"
        );
        let root_pid = fixture_pid(&fixture.pid_path());
        assert_pid_absent(root_pid);
        assert_process_group_absent(root_pid);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_capture_bound_accepts_4096_and_refuses_4097_bytes() {
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            read_version_probe_stream(
                std::io::Cursor::new(vec![b'x'; VERSION_OUTPUT_MAX_BYTES]),
                deadline
            )
            .map(|bytes| bytes.len()),
            Some(VERSION_OUTPUT_MAX_BYTES)
        );
        assert_eq!(
            read_version_probe_stream(
                std::io::Cursor::new(vec![b'x'; VERSION_OUTPUT_MAX_BYTES + 1]),
                deadline
            ),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_refuses_stdout_beyond_the_capture_bound() {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf 'codex-cli 0.144.1\\n'\ni=0\nwhile [ \"$i\" -lt 5000 ]; do printf x; i=$((i + 1)); done\n",
        );
        assert_eq!(
            run_fixture_version_probe_with_timeout(&fixture.program, Duration::from_secs(1)),
            None,
            "a valid version prefix must not bypass the output bound"
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_refuses_stderr_beyond_the_capture_bound() {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf 'codex-cli 0.144.1\\n'\ni=0\nwhile [ \"$i\" -lt 5000 ]; do printf x >&2; i=$((i + 1)); done\n",
        );
        assert_eq!(
            run_fixture_version_probe_with_timeout(&fixture.program, Duration::from_secs(1)),
            None,
            "stderr overflow must fail closed independently of valid stdout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_reader_error_fails_closed() {
        struct ErrorReader;

        impl Read for ErrorReader {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected probe reader failure"))
            }
        }

        assert_eq!(
            read_version_probe_stream(ErrorReader, Instant::now() + Duration::from_secs(1)),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_reader_error_runs_process_cleanup_before_refusal() {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nwhile :; do :; done\n",
        );
        let pid_path = fixture.pid_path();
        let mut spawn_reader = move |name: &'static str, job: VersionProbeReaderJob| {
            if name == "tachi-version-stdout" {
                drop(job);
                return std::thread::Builder::new()
                    .name(name.to_string())
                    .spawn(|| None);
            }
            std::thread::Builder::new()
                .name(name.to_string())
                .spawn(job)
        };
        assert_eq!(
            run_version_probe_with_timeout_and_spawner(
                &fixture.program,
                Duration::from_millis(300),
                &mut spawn_reader,
            ),
            None
        );
        wait_for_fixture_file(&pid_path);
        let pid = fixture_pid(&pid_path);
        assert_pid_absent(pid);
        assert_process_group_absent(pid);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_first_reader_spawn_failure_is_typed_and_cleans_process() {
        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nwhile :; do :; done\n",
        );
        let pid_path = fixture.pid_path();
        let failure_path = pid_path.clone();
        let mut spawn_reader = move |_name: &'static str, job: VersionProbeReaderJob| {
            drop(job);
            wait_for_fixture_file(&failure_path);
            Err(std::io::Error::other("injected first reader spawn failure"))
        };
        assert_eq!(
            run_version_probe_with_timeout_and_spawner(
                &fixture.program,
                Duration::from_secs(1),
                &mut spawn_reader,
            ),
            None,
            "reader spawn failure must become a typed prerequisite refusal"
        );
        let pid = fixture_pid(&pid_path);
        assert_pid_absent(pid);
        assert_process_group_absent(pid);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_second_reader_spawn_failure_joins_first_and_cleans_process() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nwhile :; do :; done\n",
        );
        let pid_path = fixture.pid_path();
        let failure_path = pid_path.clone();
        let first_reader_joined = Arc::new(AtomicBool::new(false));
        let joined = Arc::clone(&first_reader_joined);
        let mut spawn_reader = move |name: &'static str, job: VersionProbeReaderJob| {
            if name == "tachi-version-stderr" {
                drop(job);
                wait_for_fixture_file(&failure_path);
                return Err(std::io::Error::other(
                    "injected second reader spawn failure",
                ));
            }
            let joined = Arc::clone(&joined);
            std::thread::Builder::new()
                .name(name.to_string())
                .spawn(move || {
                    let result = job();
                    joined.store(true, Ordering::SeqCst);
                    result
                })
        };
        assert_eq!(
            run_version_probe_with_timeout_and_spawner(
                &fixture.program,
                Duration::from_secs(1),
                &mut spawn_reader,
            ),
            None
        );
        let pid = fixture_pid(&pid_path);
        assert_pid_absent(pid);
        assert_process_group_absent(pid);
        let join_deadline = Instant::now() + Duration::from_secs(1);
        while !first_reader_joined.load(Ordering::SeqCst) && Instant::now() < join_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            first_reader_joined.load(Ordering::SeqCst),
            "the cleanup owner must join the first reader after second-reader spawn failure"
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_reader_spawn_unwind_joins_first_and_cleans_process() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let fixture = VersionProbeFixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nwhile :; do :; done\n",
        );
        let pid_path = fixture.pid_path();
        let failure_path = pid_path.clone();
        let first_reader_joined = Arc::new(AtomicBool::new(false));
        let joined = Arc::clone(&first_reader_joined);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut spawn_reader = move |name: &'static str, job: VersionProbeReaderJob| {
                if name == "tachi-version-stderr" {
                    drop(job);
                    wait_for_fixture_file(&failure_path);
                    panic!("injected second reader spawn panic");
                }
                let joined = Arc::clone(&joined);
                std::thread::Builder::new()
                    .name(name.to_string())
                    .spawn(move || {
                        let result = job();
                        joined.store(true, Ordering::SeqCst);
                        result
                    })
            };
            let _ = run_version_probe_with_timeout_and_spawner(
                &fixture.program,
                Duration::from_secs(1),
                &mut spawn_reader,
            );
        }));
        assert!(unwind.is_err(), "the injected spawn panic must unwind");
        let pid = fixture_pid(&pid_path);
        assert_pid_absent(pid);
        assert_process_group_absent(pid);
        let join_deadline = Instant::now() + Duration::from_secs(1);
        while !first_reader_joined.load(Ordering::SeqCst) && Instant::now() < join_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            first_reader_joined.load(Ordering::SeqCst),
            "the cleanup owner must join its first reader after unwind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_joins_both_readers_when_one_thread_panics() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let second_reader_joined = Arc::new(AtomicBool::new(false));
        let stdout_reader =
            std::thread::spawn(|| -> Option<Vec<u8>> { panic!("injected stdout reader panic") });
        let joined = Arc::clone(&second_reader_joined);
        let stderr_reader = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            joined.store(true, Ordering::SeqCst);
            Some(Vec::new())
        });
        let mut readers = VersionProbeReaders {
            stdout: Some(stdout_reader),
            stderr: Some(stderr_reader),
        };

        assert_eq!(readers.join_all(), None);
        assert!(
            second_reader_joined.load(Ordering::SeqCst),
            "the second reader must be joined even when the first reader panics"
        );
    }

    /// `0.9.0` is NOT newer than `0.144.1`, even though it is as a string. This
    /// is the exact comparison bug the lane card for this seat warns about, and
    /// the reason `versions_match` goes through `version_components`.
    #[test]
    fn version_comparison_is_never_lexicographic() {
        assert!(!versions_match("0.9.0", "0.144.1"));
        assert!(versions_match("0.144.1", "0.144.1"));
        assert!(!versions_match("0.144.10", "0.144.1"));
    }
}
