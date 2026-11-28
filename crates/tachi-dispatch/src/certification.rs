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
//! Version matching governs historical certification only. Admission can also
//! use the installed Codex CLI sandbox interface, recorded separately without
//! transferring an old kill-test claim to a new binary.
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
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

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
    /// installed binary against this before claiming historical certification.
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

/// Pull the version token out of a `--version` line: `codex-cli 0.144.1` ->
/// `0.144.1`. Requires at least two dotted numeric components, so a stray `1` or
/// a binary name never passes for a version.
pub fn parse_version_output(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|token| version_components(token).is_some_and(|parts| parts.len() >= 2))
        .map(|token| token.trim_start_matches('v').to_string())
}

// ─── Runtime version and capability probes ────────────────────────────────────────────────

/// What the installed vendor binary reports, or `None` when we could not find
/// out (not on `PATH`, non-zero exit, unparseable output, or it hung).
///
/// An unknown version cannot claim historical certification or vendor sandbox
/// admission. A known newer version can use separately probed CLI capability.
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

    let key = (backend.to_string(), identity.clone());
    let (flight, leader) = {
        let mut flights = version_inflight()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = flights.get(&key).and_then(std::sync::Weak::upgrade) {
            (existing, false)
        } else {
            let flight = std::sync::Arc::new(VersionFlight {
                result: Mutex::new(None),
                complete: std::sync::Condvar::new(),
            });
            flights.insert(key.clone(), std::sync::Arc::downgrade(&flight));
            (flight, true)
        }
    };
    if !leader {
        let mut result = flight
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(version) = result.as_ref() {
                return version.clone();
            }
            result = flight
                .complete
                .wait(result)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    let probed_version = std::panic::catch_unwind(|| run_version_probe(&program)).unwrap_or(None);
    // A slow A probe is not evidence for an upgraded B, even for the callers
    // already waiting on A's flight. They can retry and resolve B explicitly.
    let version = match which(backend) {
        Some(current_program)
            if current_program == program && binary_identity(&current_program) == identity =>
        {
            if let Ok(mut cache) = cache.lock() {
                cache.insert(backend.to_string(), (identity, probed_version.clone()));
            }
            probed_version
        }
        _ => None,
    };
    *flight
        .result
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(version.clone());
    flight.complete.notify_all();
    let mut flights = version_inflight()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if flights
        .get(&key)
        .and_then(std::sync::Weak::upgrade)
        .is_some_and(|current| std::sync::Arc::ptr_eq(&current, &flight))
    {
        flights.remove(&key);
    }
    version
}

/// Probe the interface the actual launcher uses, without credentials or a model call.
/// No version allowlist or cached capability survives an in-place CLI upgrade.
pub fn probe_codex_read_only_support() -> bool {
    let Some(program) = which("codex") else {
        return false;
    };
    probe_codex_read_only_program(&program)
}

#[cfg(not(unix))]
fn probe_codex_read_only_program(_program: &std::path::Path) -> bool {
    false
}

#[cfg(unix)]
fn probe_codex_read_only_program(program: &std::path::Path) -> bool {
    probe_codex_program_with_timeout(program, std::time::Duration::from_secs(5))
}

#[cfg(unix)]
fn probe_codex_program_with_timeout(
    program: &std::path::Path,
    timeout: std::time::Duration,
) -> bool {
    let Some(output) = run_program_probe_with_timeout(program, &["exec", "--help"], timeout) else {
        return false;
    };
    output.succeeded
        && (codex_help_supports_read_only(&String::from_utf8_lossy(&output.stdout))
            || codex_help_supports_read_only(&String::from_utf8_lossy(&output.stderr)))
}

#[cfg(unix)]
const MAX_PROGRAM_PROBE_OUTPUT: usize = 65_536;

#[cfg(unix)]
struct ProgramProbeOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    succeeded: bool,
}

/// The unreaped root pins its PID/PGID while the owned group is terminated.
/// After `wait`, this type only observes group absence: a cached PGID is never
/// signalled again because the numeric ID may already belong to someone else.
#[cfg(unix)]
struct ProgramProbeChild {
    child: Option<std::process::Child>,
    owns_root: bool,
}

#[cfg(unix)]
impl ProgramProbeChild {
    fn id(&self) -> Option<u32> {
        self.child.as_ref().map(std::process::Child::id)
    }

    fn observe_exit(&mut self) -> std::io::Result<bool> {
        if !self.owns_root {
            return Err(std::io::Error::other(
                "program probe root ownership was lost",
            ));
        }
        let pid = self
            .id()
            .ok_or_else(|| std::io::Error::other("program probe root is unavailable"))?;
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(unsafe { info.si_pid() } != 0);
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.owns_root = false;
            }
            return Err(error);
        }
    }

    fn terminate_and_reap(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if !self.owns_root {
            return Err(std::io::Error::other(
                "program probe root ownership was lost",
            ));
        }
        let pgid = self
            .id()
            .ok_or_else(|| std::io::Error::other("program probe root is unavailable"))?
            as libc::pid_t;
        // SAFETY: the root is unreaped, so neither its PID nor its original PGID
        // can be recycled. Killing the direct PID also covers a root that moved
        // itself into another process group before hanging.
        unsafe {
            libc::kill(-pgid, libc::SIGSTOP);
            libc::kill(-pgid, libc::SIGKILL);
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }

        let reap_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match self.observe_exit() {
                Ok(true) => {
                    // WNOWAIT observed a zombie, so this wait cannot block on a
                    // still-running vendor process.
                    let status = self
                        .child
                        .as_mut()
                        .expect("owned probe root remains present")
                        .wait();
                    self.owns_root = false;
                    return status.map(Some);
                }
                Ok(false) if std::time::Instant::now() < reap_deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(false) => {
                    // Do not let an uninterruptible vendor process block this
                    // probe or its singleflight followers. A reaper owns the
                    // blocking wait; this path never waits again in Drop.
                    self.owns_root = false;
                    let mut child = self.child.take().expect("owned probe root remains present");
                    let _ = std::thread::Builder::new()
                        .name("tachi-program-probe-reaper".into())
                        .spawn(move || {
                            let _ = child.wait();
                        });
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(unix)]
impl Drop for ProgramProbeChild {
    fn drop(&mut self) {
        if self.owns_root {
            let _ = self.terminate_and_reap();
        }
    }
}

#[cfg(unix)]
fn program_probe_group_absent(pgid: libc::pid_t) -> bool {
    // Observation only. EPERM and a recycled live PGID both fail closed.
    let result = unsafe { libc::kill(-pgid, 0) };
    result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
fn set_program_probe_nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn spawn_program_probe_reader<R>(
    mut stream: R,
    tag: usize,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: std::sync::mpsc::Sender<(usize, std::io::Result<Vec<u8>>)>,
) -> std::io::Result<std::thread::JoinHandle<()>>
where
    R: std::io::Read + Send + 'static,
{
    std::thread::Builder::new()
        .name(format!("tachi-program-probe-{tag}"))
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 8192];
            let result = loop {
                if cancel.load(std::sync::atomic::Ordering::Acquire) {
                    break Ok(bytes);
                }
                match stream.read(&mut buffer) {
                    Ok(0) => break Ok(bytes),
                    Ok(count) => {
                        let remaining = (MAX_PROGRAM_PROBE_OUTPUT + 1).saturating_sub(bytes.len());
                        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => break Err(error),
                }
            };
            let _ = tx.send((tag, result));
        })
}

#[cfg(unix)]
fn run_program_probe_with_timeout(
    program: &std::path::Path,
    args: &[&str],
    timeout: std::time::Duration,
) -> Option<ProgramProbeOutput> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    let child = Command::new(program)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Arm ownership before any fallible pipe/thread setup. Every early return
    // below therefore terminates and reaps an already-spawned root.
    let mut process = ProgramProbeChild {
        child: Some(child),
        owns_root: true,
    };
    let pgid = process.id()? as libc::pid_t;
    let stdout = process.child.as_mut()?.stdout.take()?;
    let stderr = process.child.as_mut()?.stderr.take()?;
    set_program_probe_nonblocking(stdout.as_raw_fd()).ok()?;
    set_program_probe_nonblocking(stderr.as_raw_fd()).ok()?;

    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_program_probe_reader(stdout, 0, cancel.clone(), tx.clone()).ok()?;
    let stderr_reader = match spawn_program_probe_reader(stderr, 1, cancel.clone(), tx) {
        Ok(reader) => reader,
        Err(_) => {
            cancel.store(true, Ordering::Release);
            let _ = stdout_reader.join();
            return None;
        }
    };

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let root_observed = loop {
        match process.observe_exit() {
            Ok(true) => break true,
            Ok(false) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(false) => {
                timed_out = true;
                break true;
            }
            Err(_) => break false,
        }
    };
    let status = if root_observed {
        process.terminate_and_reap().ok().flatten()
    } else {
        None
    };
    let group_deadline = Instant::now() + Duration::from_secs(2);
    let group_absent = status.is_some()
        && loop {
            if program_probe_group_absent(pgid) {
                break true;
            }
            if Instant::now() >= group_deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };

    let output_deadline = Instant::now() + Duration::from_secs(1);
    let mut outputs = [None, None];
    let mut output_complete = true;
    for _ in 0..2 {
        let remaining = output_deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok((tag, output)) => outputs[tag] = Some(output),
            Err(_) => {
                output_complete = false;
                break;
            }
        }
    }
    cancel.store(true, Ordering::Release);
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    if timed_out || !root_observed || !output_complete || !group_absent {
        return None;
    }
    let stdout = outputs[0].take()?.ok()?;
    let stderr = outputs[1].take()?.ok()?;
    if stdout.len() > MAX_PROGRAM_PROBE_OUTPUT || stderr.len() > MAX_PROGRAM_PROBE_OUTPUT {
        return None;
    }
    Some(ProgramProbeOutput {
        stdout,
        stderr,
        succeeded: status?.success(),
    })
}

fn codex_help_supports_read_only(help: &str) -> bool {
    // Bind the value to the sandbox option's stanza; an unrelated mention of
    // read-only elsewhere in help must not qualify the launcher interface.
    let mut sandbox = false;
    for line in help.lines().map(str::trim) {
        if line.starts_with('-') {
            sandbox = line
                .split(|c: char| c.is_whitespace() || c == ',')
                .any(|token| token == "--sandbox");
        }
        if sandbox && line.starts_with("[possible values:") {
            return line
                .trim_start_matches("[possible values:")
                .trim_end_matches(']')
                .split(',')
                .any(|value| value.trim() == "read-only");
        }
    }
    false
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

type VersionFlightKey = (String, BinaryIdentity);

struct VersionFlight {
    result: Mutex<Option<Option<String>>>,
    complete: std::sync::Condvar,
}

#[allow(clippy::type_complexity)]
fn version_inflight() -> &'static Mutex<HashMap<VersionFlightKey, std::sync::Weak<VersionFlight>>> {
    static FLIGHTS: OnceLock<Mutex<HashMap<VersionFlightKey, std::sync::Weak<VersionFlight>>>> =
        OnceLock::new();
    FLIGHTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `<program> --version`, on a leash: a vendor binary that hangs must not hang
/// the dispatch, so the probe runs on a worker thread and a timeout resolves to
/// `None` (fail closed), not to a wait.
#[cfg(unix)]
fn run_version_probe(program: &std::path::Path) -> Option<String> {
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let output = run_program_probe_with_timeout(program, &["--version"], PROBE_TIMEOUT)?;
    if !output.succeeded {
        return None;
    }
    parse_version_output(&String::from_utf8_lossy(&output.stdout))
        .or_else(|| parse_version_output(&String::from_utf8_lossy(&output.stderr)))
}

#[cfg(not(unix))]
fn run_version_probe(program: &std::path::Path) -> Option<String> {
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let (tx, rx) = std::sync::mpsc::channel();
    let program = program.to_path_buf();
    std::thread::spawn(move || {
        let output = std::process::Command::new(&program)
            .arg("--version")
            .output();
        let _ = tx.send(output);
    });
    let output = rx.recv_timeout(PROBE_TIMEOUT).ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_version_output(&stdout)
        .or_else(|| parse_version_output(&String::from_utf8_lossy(&output.stderr)))
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

    #[test]
    fn sandbox_capability_requires_the_supported_option_value() {
        assert!(codex_help_supports_read_only("  -s, --sandbox <SANDBOX_MODE>\n description\n [possible values: read-only, workspace-write, danger-full-access]"));
        for help in [
            "",
            "--sandbox read-only",
            "--sandbox <MODE>\n [possible values: workspace-write]",
            "--sandbox <MODE>\n--other <MODE>\n [possible values: read-only]",
            "--sandboxed <MODE>\n [possible values: read-only]",
        ] {
            assert!(!codex_help_supports_read_only(help), "{help}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_capability_requires_a_successful_cli() {
        assert!(!probe_codex_read_only_program(std::path::Path::new(
            "/usr/bin/false"
        )));
        assert!(!probe_codex_read_only_program(std::path::Path::new(
            "/no/such/codex"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_coalesces_cold_cache_calls_for_one_binary_identity() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "tachi-version-flight-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("owned probe fixture");
        let program = root.join("vendor");
        let calls = root.join("calls");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nprintf 'call\\n' >> '{}'\nsleep 1\nprintf 'vendor 1.2.3\\n'\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();

        let program_name = program.to_string_lossy().into_owned();
        let workers = (0..8)
            .map(|_| {
                let program_name = program_name.clone();
                std::thread::spawn(move || probe_backend_version(&program_name))
            })
            .collect::<Vec<_>>();
        for worker in workers {
            assert_eq!(worker.join().unwrap(), Some("1.2.3".to_string()));
        }
        assert_eq!(
            std::fs::read_to_string(calls).unwrap().lines().count(),
            1,
            "one binary identity must have one in-flight probe"
        );
        std::fs::remove_dir_all(root).expect("remove probe fixture");
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_refuses_a_result_when_the_binary_changes_mid_flight() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "tachi-version-upgrade-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("owned upgrade fixture");
        let program = root.join("vendor");
        let started = root.join("started");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nprintf 'started\\n' > '{}'\nsleep 1\nprintf 'vendor 1.0.0\\n'\n",
                started.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let program_name = program.to_string_lossy().into_owned();
        let probe_name = program_name.clone();
        let probe = std::thread::spawn(move || probe_backend_version(&probe_name));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !started.exists() {
            assert!(std::time::Instant::now() < deadline, "probe did not start");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::fs::write(
            &program,
            "#!/bin/sh\n# a deliberately different length and identity\nprintf 'vendor 2.0.0\\n'\n",
        )
        .unwrap();
        assert_eq!(probe.join().unwrap(), None);
        assert_eq!(
            probe_backend_version(&program_name),
            Some("2.0.0".to_string())
        );
        std::fs::remove_dir_all(root).expect("remove upgrade fixture");
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_prefers_stdout_over_stderr_diagnostics() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "tachi-version-streams-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("owned stream fixture");
        let program = root.join("vendor");
        std::fs::write(
            &program,
            "#!/bin/sh\nprintf 'dependency 9.8.7\\n' >&2\nprintf 'vendor 1.2.3\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(run_version_probe(&program).as_deref(), Some("1.2.3"));
        std::fs::write(
            &program,
            "#!/bin/sh\nprintf 'no stdout version\\n'\nprintf 'vendor 4.5.6\\n' >&2\n",
        )
        .unwrap();
        assert_eq!(
            run_version_probe(&program).as_deref(),
            Some("4.5.6"),
            "stderr remains a fallback when stdout has no version"
        );
        std::fs::remove_dir_all(root).expect("remove stream fixture");
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_re_resolves_the_backend_path_before_publishing() {
        const CHILD_ENV: &str = "TACHI_VERSION_PATH_REPLACEMENT_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "certification::tests::version_probe_re_resolves_the_backend_path_before_publishing",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .expect("run PATH replacement scenario in a child process");
            assert!(
                output.status.success(),
                "child PATH scenario failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "tachi-version-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let original = std::env::var_os("PATH");
        let second_program = second.join("vendor");
        let started = root.join("started");
        std::fs::write(
            &second_program,
            format!(
                "#!/bin/sh\nprintf 'started\\n' > '{}'\nsleep 1\nprintf 'vendor 1.0.0\\n'\n",
                started.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&second_program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let search_path = original
            .as_ref()
            .map(|path| format!("{}:{}", second.display(), path.to_string_lossy()))
            .unwrap_or_else(|| second.display().to_string());
        std::env::set_var("PATH", search_path);
        let probe = std::thread::spawn(|| probe_backend_version("vendor"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !started.exists() {
            assert!(std::time::Instant::now() < deadline, "probe did not start");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let first_program = first.join("vendor");
        std::fs::write(&first_program, "#!/bin/sh\nprintf 'vendor 2.0.0\\n'\n").unwrap();
        std::fs::set_permissions(&first_program, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var(
            "PATH",
            original
                .as_ref()
                .map(|path| {
                    format!(
                        "{}:{}:{}",
                        first.display(),
                        second.display(),
                        path.to_string_lossy()
                    )
                })
                .unwrap_or_else(|| format!("{}:{}", first.display(), second.display())),
        );
        assert_eq!(probe.join().unwrap(), None);
        if let Some(original) = original {
            std::env::set_var("PATH", original);
        } else {
            std::env::remove_var("PATH");
        }
        std::fs::remove_dir_all(root).expect("remove path fixture");
    }

    #[cfg(unix)]
    #[test]
    fn program_probe_escaped_root_child() {
        if std::env::var_os("TACHI_PROGRAM_PROBE_ESCAPE_CHILD").is_none() {
            return;
        }
        let id_file = std::env::temp_dir().join(format!("tachi-probe-escape-{}.pgid", unsafe {
            libc::getppid()
        }));
        let Ok(pgid) = std::fs::read_to_string(&id_file) else {
            return;
        };
        let pgid = pgid.trim().parse::<libc::pid_t>().unwrap();
        assert_eq!(unsafe { libc::setpgid(0, pgid) }, 0);
        std::fs::write(id_file.with_extension("escaped"), b"escaped").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[cfg(unix)]
    #[test]
    fn program_probe_timeout_kills_a_root_that_left_its_original_group() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;

        let root = std::env::temp_dir().join(format!(
            "tachi-probe-escape-fixture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let id_file =
            std::env::temp_dir().join(format!("tachi-probe-escape-{}.pgid", std::process::id()));
        let witness = id_file.with_extension("escaped");
        let mut anchor = std::process::Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .process_group(0)
            .spawn()
            .expect("spawn anchor process group");
        std::fs::write(&id_file, anchor.id().to_string()).unwrap();
        let program = root.join("vendor");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nTACHI_PROGRAM_PROBE_ESCAPE_CHILD=1 exec '{}' --exact certification::tests::program_probe_escaped_root_child --nocapture --test-threads=1\n",
                std::env::current_exe().unwrap().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(run_program_probe_with_timeout(
                &program,
                &["--version"],
                std::time::Duration::from_secs(2),
            ));
        });
        let escape_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !witness.exists() && std::time::Instant::now() < escape_deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let escaped = witness.exists();
        let output = rx.recv_timeout(std::time::Duration::from_secs(10));
        let _ = anchor.kill();
        let _ = anchor.wait();
        let _ = std::fs::remove_file(&id_file);
        let _ = std::fs::remove_file(&witness);
        std::fs::remove_dir_all(root).unwrap();
        assert!(escaped, "the probe root did not enter the anchor group");
        assert!(output
            .expect("a group-escaping root must not block the probe")
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_probe_cleans_descendants_on_timeout_and_parent_exit() {
        use std::os::unix::fs::PermissionsExt;
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // Unique root per run: a leftover root from an aborted earlier run
        // must not fail the next run's directory creation (#2340).
        let run_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tachi-probe-cleanup-{}-{run_id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let program = root.join("codex");
        std::fs::write(&program, "#!/bin/sh\nprintf '%s\\n' '--sandbox <MODE>' '[possible values: read-only, workspace-write]'\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(probe_codex_program_with_timeout(
            &program,
            std::time::Duration::from_secs(2)
        ));
        for (idx, tail) in ["wait", "exit 1", "exit 0"].iter().enumerate() {
            // Per-iteration witness files: a late write from one iteration
            // can never be blamed on another iteration (#2340).
            let marker = root.join(format!("descendant-survived-{idx}"));
            let pid_file = root.join(format!("descendant-pid-{idx}"));
            let program = root.join(format!("codex-{idx}"));
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\n(sleep 20; echo leaked > '{}') &\necho $! > '{}'\n{tail}\n",
                    marker.display(),
                    pid_file.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            // "wait" holds the parent into the fixed-timeout kill path; the
            // exit tails exercise the kill after the parent already exited
            // and left the descendant orphaned inside the process group.
            let timeout = if *tail == "wait" {
                std::time::Duration::from_secs(2)
            } else {
                std::time::Duration::from_millis(50)
            };
            assert!(!probe_codex_program_with_timeout(&program, timeout));
            // Wait on the descendant's actual liveness instead of sampling
            // the filesystem at a fixed instant (#2340). The parent writes
            // the pid right after spawning the descendant, so a readable pid
            // file means the descendant existed; once the pid is gone the
            // marker can never be written. An absent pid file after the
            // grace window means the script never got to spawn anything
            // (killed while unscheduled), so there is no witness to check.
            let spawn_grace = std::time::Instant::now() + std::time::Duration::from_millis(500);
            let pid = loop {
                if let Some(pid) = std::fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|text| text.trim().parse::<i32>().ok())
                {
                    break Some(pid);
                }
                if std::time::Instant::now() >= spawn_grace {
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            if let Some(pid) = pid {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    if unsafe { kill(pid, 0) } != 0 {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "descendant {pid} still alive after probe for {tail}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                assert!(!marker.exists(), "probe descendant survived {tail}");
            }
        }
        std::fs::remove_dir_all(root).unwrap();
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
        // A bare integer is not a version; a name is not a version.
        assert_eq!(parse_version_output("codex 1"), None);
        assert_eq!(parse_version_output("no version here"), None);
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
