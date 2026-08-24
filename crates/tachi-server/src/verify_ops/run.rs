//! #1454 slice 2: server-executed verification runs.
//!
//! `tachi_verify(action='run')` is the merge-authority evidence producer.
//! Unlike `start`/`record` (where the caller asserts outcomes), `run` has the
//! daemon itself execute a closed-set check command in the flow's claimed
//! worktree and observe the result:
//!
//! 1. `flow_id` must resolve to an **active** `session_claims` WorkClaim
//!    (server-side accessor: `memcore::list_claims(..., Active)`, the same
//!    query surface `claims_ops` uses). No claim / no worktree → fail-closed
//!    typed `Err`. A claim worktree that is the server's own project root or
//!    an ancestor of it is rejected outright (F5).
//! 2. G1: at executor start the server-owned receipt store root
//!    (`<tachi-home>/verify-receipts`) must NOT resolve inside a git work
//!    tree — a verifiable inside is a typed, loud refusal (the store must
//!    stay an un-authorable location).
//! 3. G2: the candidate's claim worktree is NEVER executed. The server
//!    observes the claim HEAD (`source_head`), then creates a SERVER-OWNED
//!    detached copy (`git worktree add --detach
//!    <tachi-home>/verify-worktrees/<flow_id>-<kind>-<utc> <source_head>`)
//!    and runs the check with cwd = the copy. The copy is removed on ALL
//!    exits (RAII guard; best-effort + loud log). A claim-worktree mutation
//!    mid-run therefore cannot touch the executed tree (no pre/post snapshot
//!    TOCTOU), and the claim's own dirtiness is never consulted — the
//!    receipt binds `source_head` + copy immutability.
//! 4. `check_kind` maps to a **closed** argv table (verbatim `ci.yml`
//!    rust-job commands). There is no caller-influenced argv: any other kind
//!    is a typed `Err` naming the closed set.
//! 5. stdout+stderr are captured to
//!    `.tachi/runs/<flow_id>/verify-run-<kind>-<utc-seconds>.log`, the run is
//!    killed on timeout (default 1800s / cap 3600s, provisional dispatch
//!    clause 9) and the post-kill wait is bounded by a second short cap (F7);
//!    on its expiry the child is abandoned with `kill_abandoned` recorded and
//!    the in-flight guard released via RAII.
//! 6. Post-run integrity is observed ON THE COPY: `git status --porcelain`
//!    must be empty and `git rev-parse HEAD` must still equal `source_head`;
//!    otherwise the run is `failed` with the typed reason
//!    `tree_mutation_during_run` (never passed). The spawned argv carries
//!    `CARGO_TARGET_DIR=<tachi-home>/verify-target` so the candidate config
//!    cannot redirect the build into the copy (no other env scrubbing in this
//!    leaf — the candidate runs under the server UID regardless, a named
//!    residual, #1454 C1). G4: the tool's version (`cargo --version` /
//!    `cargo audit --version` / `python3 --version`) is captured into the
//!    receipt as informational `tool_version`; no gate comparison.
//! 7. The AUTHORITY write is the receipt in the server-owned store
//!    (`<tachi-home>/verify-receipts/<flow_id>/<kind>.json`, F1) — outside
//!    any repository. The ledger display item is written through
//!    [`super::ledger::record_server_run_item`] with `source:
//!    server_run:<kind>` as display-only metadata; the gate reads the receipt
//!    store, never the ledger `source` string (#1454 F1).
//! 8. G3: the run RESPONSE's top-level `overall` is the gate overall
//!    (evaluated with the just-observed head); the raw ledger value moves to
//!    the `ledger_overall` detail field.
//!
//! Per-flow serialization is a minimal in-process guard (a static
//! `HashSet<String>` of in-flight flow ids); a second concurrent run for the
//! same flow is a typed `Err` (`already running`). No frameworks.

use super::receipt_store::write_run_receipt;
use super::storage::now;
use super::*;
use crate::server_state::MemoryServer;
use memcore::ClaimState;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// Default per-run timeout — provisional (dispatch clause 9).
const DEFAULT_RUN_TIMEOUT_SECS: u64 = 1800;
/// Hard cap on a caller-supplied `timeout_secs` — provisional (dispatch
/// clause 9). A caller cannot relax the cap; only lower it.
const MAX_RUN_TIMEOUT_SECS: u64 = 3600;
/// Second hard cap (F7): once the primary timeout fires, the post-kill wait
/// may not run unbounded even if the child ignores the kill. On expiry the
/// child is abandoned (logged loudly; `kill_abandoned` recorded).
const POST_KILL_WAIT_CAP: Duration = Duration::from_secs(5);
/// Bound on joining the stdout/stderr pump tasks. Pumps must never be awaited
/// without bound on any path (F7): a child that ignores the kill keeps its
/// pipes open, so the pumps cannot be joined indefinitely.
const PUMP_JOIN_GRACE: Duration = Duration::from_secs(5);

/// Closed `check_kind` → argv table. Verbatim `ci.yml` rust-job commands:
/// - `version-sync` → `python3 scripts/check_release_versions.py`
/// - `clippy` → `cargo clippy --workspace --all-targets --locked -- -D warnings`
/// - `fmt` → `cargo fmt --all --check`
/// - `audit` → `cargo audit --deny warnings`
/// - `nextest` → `cargo nextest run --workspace --locked --profile ci`
/// - `portable-contract` → `cargo test -p portable-kernel --features portable-contract-test --locked`
/// - `doc` → `cargo test --workspace --locked --doc`
///
/// No other kind exists; no caller-supplied argv is ever accepted.
pub(crate) fn check_kind_argv(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "version-sync" => Some(&["python3", "scripts/check_release_versions.py"]),
        "clippy" => Some(&[
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]),
        "fmt" => Some(&["cargo", "fmt", "--all", "--check"]),
        "audit" => Some(&["cargo", "audit", "--deny", "warnings"]),
        "nextest" => Some(&[
            "cargo",
            "nextest",
            "run",
            "--workspace",
            "--locked",
            "--profile",
            "ci",
        ]),
        "portable-contract" => Some(&[
            "cargo",
            "test",
            "-p",
            "portable-kernel",
            "--features",
            "portable-contract-test",
            "--locked",
        ]),
        "doc" => Some(&["cargo", "test", "--workspace", "--locked", "--doc"]),
        _ => None,
    }
}

const CLOSED_KINDS: &str = "version-sync, clippy, fmt, audit, nextest, portable-contract, doc";

fn unknown_kind_error(kind: &str) -> String {
    format!("unknown check_kind '{kind}' for tachi_verify run; closed set: {CLOSED_KINDS}")
}

/// Outcome of one spawned check process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckRunOutcome {
    /// `Some(0)` → `passed`; anything else (nonzero, signal, timeout) →
    /// `failed`.
    pub(crate) exit_code: Option<i32>,
    pub(crate) duration_ms: u64,
    pub(crate) timed_out: bool,
    /// `true` when the post-kill wait expired and the child was abandoned
    /// (F7). The in-flight guard is RAII-released regardless.
    pub(crate) kill_abandoned: bool,
}

/// Seam over a spawned child so the timeout/kill/bound discipline
/// ([`wait_with_kill_cap`]) is testable with a fake child that ignores
/// kills (F7 unit test).
#[async_trait::async_trait]
pub(crate) trait CheckChild: Send {
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus>;
    async fn start_kill(&mut self) -> std::io::Result<()>;
}

#[async_trait::async_trait]
impl CheckChild for tokio::process::Child {
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        tokio::process::Child::wait(self).await
    }
    async fn start_kill(&mut self) -> std::io::Result<()> {
        tokio::process::Child::start_kill(self)
    }
}

/// #1454 F7 timeout discipline, shared by the real runner and test fakes:
///
/// 1. Wait up to `timeout` for the child.
/// 2. On timeout: attempt `start_kill` (result ignored — the kill may
///    fail), then bound the post-kill reaping wait with `post_kill_cap`.
/// 3. If the child still has not exited when `post_kill_cap` expires,
///    abandon it: return with `kill_abandoned = true` instead of waiting
///    forever. The caller (run_with_runner) keeps the in-flight guard on an
///    RAII drop, so an abandoned child never wedges the per-flow slot.
pub(crate) async fn wait_with_kill_cap<C: CheckChild>(
    child: &mut C,
    timeout: Duration,
    post_kill_cap: Duration,
) -> Result<CheckRunOutcome, String> {
    let started = Instant::now();
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(CheckRunOutcome {
            exit_code: status.code(),
            duration_ms: started.elapsed().as_millis() as u64,
            timed_out: false,
            kill_abandoned: false,
        }),
        Ok(Err(err)) => Err(format!("wait child: {err}")),
        Err(_timeout) => {
            // Best-effort kill; the result does not gate the bounded reaping
            // wait below. kill_on_drop is the belt for the real runner.
            let _ = child.start_kill().await;
            match tokio::time::timeout(post_kill_cap, child.wait()).await {
                Ok(Ok(status)) => Ok(CheckRunOutcome {
                    exit_code: status.code(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    timed_out: true,
                    kill_abandoned: false,
                }),
                Ok(Err(err)) => Err(format!("reap timed-out child: {err}")),
                Err(_abandoned) => Ok(CheckRunOutcome {
                    exit_code: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    timed_out: true,
                    kill_abandoned: true,
                }),
            }
        }
    }
}

/// Process seam: the spawn+wait surface is injectable so unit tests use a
/// canned fake (fixed exit code + a canned log file) instead of a real cargo
/// invocation. Process spawns live behind the seam; the copy-lifecycle
/// methods are SYNCHRONOUS (single `git` subprocess each) so the RAII
/// cleanup guard can call them from `Drop` without an executor.
#[async_trait::async_trait]
pub(crate) trait CheckRunner: Send + Sync {
    /// Server-observed HEAD of `worktree` (`git rev-parse HEAD`). Failure is
    /// a typed `Err` — a worktree without a resolvable HEAD cannot produce a
    /// `source_head`, and the item must not be written with a fabricated one.
    async fn observe_head(&self, worktree: &Path) -> Result<String, String>;

    /// Server-observed cleanliness of `worktree` (`git status --porcelain`
    /// must be empty). Post-run this is observed ON THE COPY (G2).
    async fn worktree_is_clean(&self, worktree: &Path) -> Result<bool, String>;

    /// Spawn `argv` in `cwd`, streaming stdout+stderr to `log_path`, killing
    /// on `timeout` (best-effort) with the F7 bounded post-kill wait. `env`
    /// overrides the inherited environment for the child (currently
    /// `CARGO_TARGET_DIR` so the candidate config cannot redirect the build).
    /// Returns the process outcome.
    async fn run_check(
        &self,
        argv: &[&str],
        cwd: &Path,
        timeout: Duration,
        log_path: &Path,
        env: &[(&str, &str)],
    ) -> Result<CheckRunOutcome, String>;

    /// G2: create a SERVER-OWNED detached copy of `claim`'s worktree at
    /// `observed_head`, rooted at `dest`
    /// (`git -C <claim> worktree add --detach <dest> <observed-head>`).
    /// Sync: one subprocess, no pipes to drain.
    fn create_detached_copy(
        &self,
        claim: &Path,
        observed_head: &str,
        dest: &Path,
    ) -> Result<(), String>;

    /// G2: remove the detached copy (`git -C <claim> worktree remove --force
    /// <copy>`). Sync so the RAII guard can run it from `Drop`; best-effort at
    /// the call sites (a failure is logged loudly, never propagated).
    fn remove_detached_copy(&self, claim: &Path, copy: &Path) -> Result<(), String>;

    /// #1454 H1: is `copy` currently registered in `claim`'s worktree list
    /// (`git -C <claim> worktree list --porcelain`)? The cleanup guard
    /// consults this BEFORE removing: a PARTIAL `git worktree add` (e.g. a
    /// post-checkout hook exiting nonzero) can leave the copy registered
    /// without the on-disk dir, or the dir without a registration — each
    /// must be cleaned separately.
    ///
    /// #1454 O1: an unanswerable git (spawn failure / nonzero exit /
    /// unparseable output) is [`RegistrationProbe::Unknown`], never a silent
    /// `NotRegistered` — the guard must not conclude "no registration" and
    /// leave the residue registered and invisible.
    fn copy_is_registered(&self, claim: &Path, copy: &Path) -> RegistrationProbe;

    /// G4: capture the kind's tool version (`cargo --version` for cargo
    /// kinds, `cargo audit --version` for audit, `python3 --version` for
    /// version-sync). Informational only — a failure yields `Ok(None)`, never
    /// a run failure.
    fn tool_version(&self, kind: &str) -> Result<Option<String>, String>;
}

/// Production runner: real tokio processes (and sync git subprocesses for the
/// copy lifecycle).
pub(crate) struct ProcessCheckRunner;

#[async_trait::async_trait]
impl CheckRunner for ProcessCheckRunner {
    async fn observe_head(&self, worktree: &Path) -> Result<String, String> {
        let output = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(worktree)
            .output()
            .await
            .map_err(|e| {
                format!(
                    "git rev-parse HEAD in {} failed to spawn: {e}",
                    worktree.display()
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "git rev-parse HEAD in {} failed: {}",
                worktree.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if head.is_empty() {
            return Err(format!(
                "git rev-parse HEAD in {} returned empty output",
                worktree.display()
            ));
        }
        Ok(head)
    }

    async fn worktree_is_clean(&self, worktree: &Path) -> Result<bool, String> {
        let output = tokio::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(worktree)
            .output()
            .await
            .map_err(|e| {
                format!(
                    "git status --porcelain in {} failed to spawn: {e}",
                    worktree.display()
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "git status --porcelain in {} failed: {}",
                worktree.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().is_empty())
    }

    async fn run_check(
        &self,
        argv: &[&str],
        cwd: &Path,
        timeout: Duration,
        log_path: &Path,
        env: &[(&str, &str)],
    ) -> Result<CheckRunOutcome, String> {
        if let Some(parent) = log_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let log = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .await
            .map_err(|e| format!("open {}: {e}", log_path.display()))?;
        let writer = std::sync::Arc::new(tokio::sync::Mutex::new(log));

        // Header line first: the log names the command and cwd even when the
        // check itself produces zero output (e.g. a green `cargo fmt --all
        // --check`), so a run log is never empty and always self-describing.
        {
            let mut guard = writer.lock().await;
            let header = format!(
                "# tachi_verify run: {} (cwd: {})\n",
                argv.join(" "),
                cwd.display()
            );
            (*guard)
                .write_all(header.as_bytes())
                .await
                .map_err(|e| format!("write run log header: {e}"))?;
        }

        let mut child = tokio::process::Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .envs(env.iter().copied())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn {} in {}: {e}", argv[0], cwd.display()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("no stdout pipe on {}", argv[0]))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("no stderr pipe on {}", argv[0]))?;

        let pump = |mut stream: Box<dyn AsyncRead + Unpin + Send>| {
            let writer = std::sync::Arc::clone(&writer);
            tokio::spawn(async move {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    let mut guard = writer.lock().await;
                    (*guard)
                        .write_all(&buf[..n])
                        .await
                        .map_err(|e| e.to_string())?;
                }
                Ok::<(), String>(())
            })
        };
        let stdout_pump = pump(Box::new(stdout) as Box<dyn AsyncRead + Unpin + Send>);
        let stderr_pump = pump(Box::new(stderr) as Box<dyn AsyncRead + Unpin + Send>);

        let mut outcome = wait_with_kill_cap(&mut child, timeout, POST_KILL_WAIT_CAP).await?;
        // Pipes close once the child exits; both pumps then drain and finish.
        // Bound the join anyway (F7): an abandoned child keeps its pipes open,
        // so the pumps must not be awaited without bound. On expiry the pump
        // tasks are detached (the child's kill_on_drop closes the pipes).
        for pump in [stdout_pump, stderr_pump] {
            match tokio::time::timeout(PUMP_JOIN_GRACE, pump).await {
                Ok(joined) => joined
                    .map_err(|e| format!("run output pump failed: {e}"))?
                    .map_err(|e| {
                        format!(
                            "failed to capture run output to {}: {e}",
                            log_path.display()
                        )
                    })?,
                Err(_) => {
                    tracing::warn!(
                        log = %log_path.display(),
                        "run output pump did not finish within {PUMP_JOIN_GRACE:?}; \
                         detaching pump tasks (abandoned child keeps pipes open)"
                    );
                    outcome.kill_abandoned = true;
                }
            }
        }
        Ok(outcome)
    }

    fn create_detached_copy(
        &self,
        claim: &Path,
        observed_head: &str,
        dest: &Path,
    ) -> Result<(), String> {
        // G2: the copy is registered in the CLAIM repo's worktree list, so a
        // failed copy leaves nothing registered. `--detach` pins the checkout
        // to the observed head (no branch).
        let output = std::process::Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(dest)
            .arg(observed_head)
            .current_dir(claim)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .map_err(|e| {
                format!(
                    "git worktree add in {} failed to spawn: {e}",
                    claim.display()
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "git worktree add --detach {} {} in {} failed: {}",
                dest.display(),
                observed_head,
                claim.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    fn remove_detached_copy(&self, claim: &Path, copy: &Path) -> Result<(), String> {
        let output = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(copy)
            .current_dir(claim)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .map_err(|e| {
                format!(
                    "git worktree remove in {} failed to spawn: {e}",
                    claim.display()
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "git worktree remove --force {} in {} failed: {}",
                copy.display(),
                claim.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    fn copy_is_registered(&self, claim: &Path, copy: &Path) -> RegistrationProbe {
        let output = match std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain", "-z"])
            .current_dir(claim)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
        {
            Ok(output) => output,
            Err(err) => {
                // #1454 O1: spawn failure is UNKNOWN, never "no registration".
                return RegistrationProbe::Unknown(format!(
                    "git worktree list in {} failed to spawn: {err}",
                    claim.display()
                ));
            }
        };
        if !output.status.success() {
            // #1454 O1: nonzero exit is UNKNOWN, never "no registration".
            return RegistrationProbe::Unknown(format!(
                "git worktree list in {} exited nonzero: {}",
                claim.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        // #1454 P1: parse `--porcelain -z` — each FIELD is NUL-terminated and
        // each record begins `worktree <path>`, so a NEWLINE inside a path
        // (e.g. a TACHI_HOME-derived copy path) stays inside its own field
        // instead of splitting the record. The pre-P1 `.lines()` parse read a
        // newline-bearing registration as NotRegistered — the oracle's silent
        // leak (reproduced against real git: `git worktree add` accepts such
        // paths). Empty fields are the NUL record separators and are skipped.
        // #1454 O1: canonicalize BOTH sides of the comparison — git porcelain
        // emits CANONICALIZED paths (e.g. /private/var/... on macOS) while
        // `copy` is the uncanonical construction from `tachi_home`
        // (e.g. /var/...). Each side is canonicalized best-effort (a missing
        // leaf falls back to its nearest existing ancestor + suffix, so the
        // registration-only-residue case — dir gone, registration persists —
        // still converges on the same canonical spelling); on failure the raw
        // spelling stays in the comparison set. The comparison succeeds if
        // ANY of {raw-equal, canon-equal} holds in either direction.
        let copy_canonical = canonical_path_best_effort(copy);
        let matched = String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .any(|field| {
                let Some(registered) = field.strip_prefix("worktree ") else {
                    return false;
                };
                let registered_path = Path::new(registered);
                let registered_canonical = canonical_path_best_effort(registered_path);
                worktree_paths_match(
                    registered_path,
                    copy,
                    registered_canonical,
                    copy_canonical.as_deref(),
                )
            });
        if matched {
            RegistrationProbe::Registered
        } else {
            RegistrationProbe::NotRegistered
        }
    }

    fn tool_version(&self, kind: &str) -> Result<Option<String>, String> {
        let Some((program, args)) = tool_version_argv(kind) else {
            return Ok(None);
        };
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("{program} --version failed to spawn: {e}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        let version = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            if output.stderr.is_empty() {
                String::new()
            } else {
                format!(" {}", String::from_utf8_lossy(&output.stderr).trim())
            }
        );
        let version = version.trim().to_string();
        Ok((!version.is_empty()).then_some(version))
    }
}

/// G4: the tool whose version is captured for a kind. Cargo kinds capture
/// `cargo --version`; `audit` captures `cargo audit --version`;
/// `version-sync` captures `python3 --version`. Informational parity only —
/// there is no gate comparison (the ci-pinned installer step is CI tooling,
/// not a runnable check).
fn tool_version_argv(kind: &str) -> Option<(&'static str, &'static [&'static str])> {
    match kind {
        "version-sync" => Some(("python3", &["--version"])),
        "audit" => Some(("cargo", &["audit", "--version"])),
        _ => Some(("cargo", &["--version"])),
    }
}

// ─── Per-flow serialization ──────────────────────────────────────────────────

fn in_flight_flows() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII release: the flow id leaves the in-flight set when the run completes
/// (success or typed `Err`).
struct FlowRunGuard {
    flow_id: String,
}

impl Drop for FlowRunGuard {
    fn drop(&mut self) {
        if let Ok(mut flows) = in_flight_flows().lock() {
            flows.remove(&self.flow_id);
        }
    }
}

fn acquire_flow_run_guard(flow_id: &str) -> Result<FlowRunGuard, String> {
    let mut flows = in_flight_flows()
        .lock()
        .map_err(|_| "verification run serialization lock poisoned".to_string())?;
    if !flows.insert(flow_id.to_string()) {
        return Err(format!(
            "verification run already in progress for flow {flow_id}"
        ));
    }
    Ok(FlowRunGuard {
        flow_id: flow_id.to_string(),
    })
}

// ─── G1: receipts root must not resolve inside a git worktree ───────────────

/// First existing ancestor of `path` (the path itself when it exists). Git
/// containment is monotonic down the tree: if the nearest existing ancestor
/// is inside a worktree, the not-yet-created `verify-receipts` dir would be
/// inside one too, so probing the ancestor answers the same question honestly.
fn first_existing_ancestor(path: &Path) -> PathBuf {
    let mut probe = path.to_path_buf();
    loop {
        if probe.exists() {
            return probe;
        }
        match probe.parent() {
            Some(parent) if parent != probe => probe = parent.to_path_buf(),
            _ => return probe,
        }
    }
}

/// G1 probe: is `probe` inside a git work tree?
///
/// Primary detection: `git -C <probe> rev-parse --is-inside-work-tree`. Only
/// a VERIFIABLE git answer short-circuits: exit 0 + `true` trips the guard,
/// exit 0 + `false` is a verifiable outside (allowed). `GIT_DIR`/
/// `GIT_WORK_TREE` are removed so ambient environment cannot distort git's
/// answer.
///
/// #1454 H3: a nonzero git exit (broken repo metadata — e.g. a
/// linked-worktree-style `.git` FILE whose `gitdir:` target is missing) or
/// an unparseable answer is NOT an outside; the walk-up `.git` fallback runs
/// instead (fail-closed — git being broken must not silently allow the
/// receipt store into a repo-shaped location).
///
/// Fallback when `git` cannot produce a verifiable answer: a walk-up `.git`
/// entry detection (an entry that is a directory, a file — a linked
/// worktree stores `.git` as a file pointing at the common gitdir — or a
/// SYMLINK, dangling or not).
///
/// #1454 O3: the probe uses `symlink_metadata` (NO follow). `Path::exists()`
/// follows the symlink, so a DANGLING `.git` symlink read as absent and the
/// walk-up let the guard through — with `symlink_metadata` the entry counts
/// as PRESENT regardless of what it points at (or whether that target
/// exists), and the guard refuses.
///
/// #1454 P3: only `NotFound` means "no .git entry here — keep walking". ANY
/// other `symlink_metadata` error — an unreadable ancestor (PermissionDenied
/// etc.) that may well CONTAIN a `.git` entry — fails CLOSED with a typed
/// error naming the unreadable ancestor; walking past it would let the
/// receipts root land in a repo-shaped authorable location the guard was
/// built to refuse.
fn probe_is_inside_git_worktree(probe: &Path) -> Result<bool, String> {
    match std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(probe)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
    {
        Ok(output) if output.status.success() => {
            let answer = String::from_utf8_lossy(&output.stdout);
            let answer = answer.trim();
            if answer == "true" {
                return Ok(true);
            }
            if answer == "false" {
                return Ok(false);
            }
            // Unparseable answer → walk-up fallback (never assume outside).
        }
        Ok(_) => {}  // nonzero git exit → walk-up fallback
        Err(_) => {} // git absent on this host → walk-up fallback
    }
    let mut ancestor = Some(probe.to_path_buf());
    while let Some(dir) = ancestor {
        match dir.join(".git").symlink_metadata() {
            Ok(_) => return Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                // #1454 P3: fail CLOSED — the ancestor could not be
                // inspected, so "no .git here" cannot be honestly concluded.
                return Err(format!(
                    "cannot inspect {} while walking up from {}: {err}; treating the \
                     location as inside a git worktree rather than failing open (#1454)",
                    dir.display(),
                    probe.display()
                ));
            }
        }
        ancestor = dir.parent().map(Path::to_path_buf);
    }
    Ok(false)
}

/// #1454 G1: the server-owned receipt store root must not resolve inside a
/// git work tree — an authorable location would let a repo manipulate the
/// merge-authority store it "owns". Runs at executor start only.
fn ensure_receipts_root_not_in_repo(tachi_home: &Path) -> Result<(), String> {
    let root = super::receipt_store::verify_receipts_root(tachi_home);
    let probe = first_existing_ancestor(&root);
    if probe_is_inside_git_worktree(&probe)? {
        return Err(format!(
            "verify receipts root {} resolves inside a git worktree — refusing to use an \
             authorable location (#1454)",
            root.display()
        ));
    }
    Ok(())
}

// ─── G2: detached-copy RAII cleanup ─────────────────────────────────────────

/// #1454 O1: the outcome of a worktree-registration probe
/// (`git worktree list --porcelain`). An unanswerable git (spawn failure,
/// nonzero exit, unparseable output) is `Unknown` — the cleanup guard must
/// treat that as "registration may exist" (fail-loud), never silently
/// conclude "no registration" and skip the registration cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegistrationProbe {
    Registered,
    NotRegistered,
    /// git could not produce a verifiable answer; `reason` names the failure
    /// for the loud log.
    Unknown(String),
}

/// Best-effort canonicalization that survives a missing leaf: canonicalize
/// the nearest EXISTING ancestor and re-append the missing suffix. This is
/// what makes the #1454 O1 registration-only-residue case (copy dir gone,
/// registration still in `worktree list --porcelain`) detectable across the
/// macOS `/var` ↔ `/private/var` aliasing: git emits the canonical spelling,
/// the construction is the uncanonical one, and NEITHER leaf exists anymore —
/// so a plain `canonicalize()` fails on both and only ancestor resolution
/// converges them. `None` when no ancestor canonicalizes.
fn canonical_path_best_effort(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    let mut probe = path.to_path_buf();
    let mut missing_suffix = Vec::new();
    loop {
        match probe.parent() {
            Some(parent) if parent != probe => {
                if let Some(name) = probe.file_name() {
                    missing_suffix.push(name.to_os_string());
                }
                probe = parent.to_path_buf();
            }
            _ => return None,
        }
        if let Ok(canonical) = probe.canonicalize() {
            let mut resolved = canonical;
            for part in missing_suffix.iter().rev() {
                resolved.push(part);
            }
            return Some(resolved);
        }
    }
}

/// #1454 O1: does the registered path (git's canonical spelling) match the
/// copy construction (the uncanonical `<tachi-home>`-rooted spelling)? The
/// canonical forms are passed in precomputed (best-effort, `None` when the
/// path cannot be canonicalized) and the comparison succeeds if ANY of
/// {raw-equal, canon-equal} holds in either direction:
/// - raw-equal: the raw spellings are identical;
/// - canon-equal: the canonical forms are identical;
/// - cross directions: one side's canonical form equals the other side's raw
///   spelling (covers the case where one side still canonicalizes and the
///   other is already in the canonical spelling).
fn worktree_paths_match(
    registered: &Path,
    copy: &Path,
    registered_canonical: Option<PathBuf>,
    copy_canonical: Option<&Path>,
) -> bool {
    if registered == copy {
        return true;
    }
    match (registered_canonical.as_deref(), copy_canonical) {
        (Some(registered_real), Some(copy_real)) => registered_real == copy_real,
        (Some(registered_real), None) => registered_real == copy,
        (None, Some(copy_real)) => copy_real == registered,
        (None, None) => false,
    }
}

/// RAII cleanup for the server-owned detached copy (G2): removed on ALL
/// exits — success, typed `Err`, timeout/abandon — via the synchronous
/// runner seam (a single `git worktree remove --force` subprocess). Removal
/// is best-effort: a failure is logged loudly and never masks the run
/// outcome.
///
/// #1454 H1: the guard is installed BEFORE the `git worktree add` attempt, so
/// cleanup covers PARTIAL creation too — a `post-checkout` hook exiting
/// nonzero makes the add fail while leaving the copy registered AND/OR on
/// disk. Registration and the on-disk dir can each exist without the other:
/// the guard removes the registration first, then the dir, then loudly
/// reports any residue. Leftover `<tachi-home>/verify-worktrees/<flow_id>-
/// <kind>-<utc>` dirs are named for the worktree sweeper (`tachi-clean
/// wt-remove` family) to reclaim.
struct DetachedCopyGuard<'a, R: CheckRunner> {
    runner: &'a R,
    claim: PathBuf,
    copy: PathBuf,
}

impl<R: CheckRunner> Drop for DetachedCopyGuard<'_, R> {
    fn drop(&mut self) {
        let probe = self.runner.copy_is_registered(&self.claim, &self.copy);
        match &probe {
            RegistrationProbe::Registered => {
                if let Err(err) = self.runner.remove_detached_copy(&self.claim, &self.copy) {
                    tracing::warn!(
                        claim = %self.claim.display(),
                        copy = %self.copy.display(),
                        "verify run cleanup failed to remove detached copy registration: {err}"
                    );
                }
            }
            // #1454 O1: an unanswerable git must NOT conclude "no
            // registration" silently — the residue would be left registered
            // and invisible. Fail-loud: warn with the reason, still attempt
            // the registration removal (best-effort), fall back to the
            // dir-existence cleanup, and name the sweeper for reclaim.
            RegistrationProbe::Unknown(reason) => {
                tracing::warn!(
                    claim = %self.claim.display(),
                    copy = %self.copy.display(),
                    "verify run cleanup could not determine whether the detached copy is \
                     registered (git unanswerable: {reason}); treating it as registered — \
                     attempting registration removal, then dir cleanup; leftover \
                     <tachi-home>/verify-worktrees/<flow_id>-<kind>-<utc> registrations/dirs \
                     are named for the worktree sweeper (`tachi-clean wt-remove`) to reclaim"
                );
                if let Err(err) = self.runner.remove_detached_copy(&self.claim, &self.copy) {
                    tracing::warn!(
                        claim = %self.claim.display(),
                        copy = %self.copy.display(),
                        "verify run cleanup best-effort registration removal also failed: {err}"
                    );
                }
            }
            RegistrationProbe::NotRegistered => {}
        }
        if self.copy.exists() {
            if let Err(err) = std::fs::remove_dir_all(&self.copy) {
                tracing::warn!(
                    claim = %self.claim.display(),
                    copy = %self.copy.display(),
                    "verify run cleanup failed to remove detached copy dir: {err}"
                );
            }
        }
        if self.copy.exists() {
            tracing::warn!(
                claim = %self.claim.display(),
                copy = %self.copy.display(),
                "verify run cleanup LEFT the detached copy DIR behind \
                 (leftover <tachi-home>/verify-worktrees/<flow_id>-<kind>-<utc> dirs are named \
                 for the worktree sweeper to reclaim)"
            );
        } else {
            // #1454 O1: the residue re-probe is fail-loud too — an
            // unanswerable git cannot certify "no registration remains".
            match self.runner.copy_is_registered(&self.claim, &self.copy) {
                RegistrationProbe::Registered => {
                    tracing::warn!(
                        claim = %self.claim.display(),
                        copy = %self.copy.display(),
                        "verify run cleanup LEFT the detached copy REGISTERED in the claim repo"
                    );
                }
                RegistrationProbe::Unknown(reason) => {
                    tracing::warn!(
                        claim = %self.claim.display(),
                        copy = %self.copy.display(),
                        "verify run cleanup could not re-verify whether a detached-copy \
                         registration remains (git still unanswerable: {reason}); a leftover \
                         registration would be named for the worktree sweeper \
                         (`tachi-clean wt-remove`) to reclaim"
                    );
                }
                RegistrationProbe::NotRegistered => {}
            }
        }
    }
}

// ─── Claim resolution ────────────────────────────────────────────────────────

/// Reject a claim worktree that is the server's own project root or an
/// ancestor of it (#1454 F5). The server knows its own root the same way the
/// run-root resolver does: the process-cached git root of the daemon's
/// working directory (`path_utils::cached_git_root`). Running checks against
/// the server's own checkout (or anything above it) would let a claim turn
/// the serving tree into a test target; that is a typed `Err`, not a managed
/// allowlist (no fragile root list — named residual risk in the PR).
fn reject_server_root_claim(worktree: &Path) -> Result<(), String> {
    let Some(server_root) = crate::path_utils::cached_git_root().cloned() else {
        return Ok(());
    };
    let canonical = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf());
    let canonical_root = server_root.canonicalize().unwrap_or(server_root);
    if canonical == canonical_root || canonical_root.starts_with(&canonical) {
        return Err(format!(
            "claim worktree {} is the server's own project root or an ancestor of it; \
             refusing to run verification in the server's own checkout",
            worktree.display()
        ));
    }
    Ok(())
}

/// Resolve the flow's ACTIVE WorkClaim worktree via the memcore
/// `session_claims` query surface (`memcore::list_claims(..., Active)` — the
/// server-side accessor `claims_ops` uses), filtered by `flow_id`. The stored
/// `worktree_path` was canonicalized at claim time
/// (`claims_ops::canonical_claim_worktree_path`, claims_ops.rs:103-108).
///
/// Fail-closed: no active claim, or an active claim without a worktree, is a
/// typed `Err` — the executor never invents a working directory to run in.
fn resolve_flow_worktree(server: &MemoryServer, flow_id: &str) -> Result<PathBuf, String> {
    let claims = server.with_global_store_read(|store| {
        memcore::list_claims(store.connection(), Some(ClaimState::Active))
            .map_err(|err| err.to_string())
    })?;
    let claim = claims
        .iter()
        .find(|claim| claim.flow_id.as_deref() == Some(flow_id))
        .ok_or_else(|| {
            format!("verification run requires an active claim with a worktree for flow {flow_id}")
        })?;
    let worktree = claim
        .worktree_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("verification run requires an active claim with a worktree for flow {flow_id}")
        })?;
    reject_server_root_claim(&worktree)?;
    Ok(worktree)
}

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Server-executed verification run (#1454 slice 2). The handler validates
/// `flow_id` + `check_kind` presence; everything else (argv, head_sha, exit
/// code, log path) is produced here from server-observed state. Caller params
/// such as `head_sha`/`status` are structurally unreachable: this signature
/// does not accept them.
pub(crate) async fn run_verification_check(
    server: &MemoryServer,
    flow_id: &str,
    check_kind: &str,
    timeout_secs: Option<u64>,
) -> Result<Value, String> {
    let argv = check_kind_argv(check_kind).ok_or_else(|| unknown_kind_error(check_kind))?;
    let timeout = Duration::from_secs(
        timeout_secs
            .unwrap_or(DEFAULT_RUN_TIMEOUT_SECS)
            .min(MAX_RUN_TIMEOUT_SECS),
    );
    run_with_runner(
        server,
        &ProcessCheckRunner,
        flow_id,
        check_kind,
        argv,
        timeout,
    )
    .await
}

async fn run_with_runner<R: CheckRunner>(
    server: &MemoryServer,
    runner: &R,
    flow_id: &str,
    check_kind: &str,
    argv: &[&str],
    timeout: Duration,
) -> Result<Value, String> {
    let worktree = resolve_flow_worktree(server, flow_id)?;
    let _guard = acquire_flow_run_guard(flow_id)?;
    let tachi_home = server.tachi_home_dir();

    // #1454 G1: the server-owned receipt store must not resolve inside a git
    // worktree (executor start, and only there).
    ensure_receipts_root_not_in_repo(&tachi_home)?;

    // Server-observed claim HEAD — the head this run binds to (G2). The claim
    // worktree itself is NEVER executed, so its dirtiness is never consulted;
    // the receipt binds `source_head` + copy immutability.
    let source_head = runner.observe_head(&worktree).await?;
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let utc_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let log_path = run_dir.join(format!("verify-run-{check_kind}-{utc_secs}.log"));

    // #1454 G2: create a SERVER-OWNED detached copy of the observed head and
    // run the check with cwd = the copy. A claim-worktree mutation mid-run
    // cannot touch the executed tree (no pre/post snapshot TOCTOU), and the
    // copy is removed on every exit by the RAII guard.
    //
    // #1454 H1: the guard is installed BEFORE the add attempt — a PARTIAL
    // `git worktree add` (e.g. a post-checkout hook exiting nonzero) leaves
    // the copy registered AND/OR on disk, and the guard must clean both up
    // on the failed-add error path.
    let copy_dir = tachi_home
        .join("verify-worktrees")
        .join(format!("{flow_id}-{check_kind}-{utc_secs}"));
    // #1454 P1 defense in depth: a copy path containing a newline/carriage
    // return is never legitimate here (a TACHI_HOME/claim path with control
    // characters), and a registration minted under it would break the
    // porcelain registration parsing — refuse LOUDLY before any
    // `git worktree add` instead of silently leaking a registration.
    if let Some(ch) = copy_dir
        .to_string_lossy()
        .chars()
        .find(|c| matches!(c, '\n' | '\r'))
    {
        return Err(format!(
            "refusing to create the verification detached copy: {} contains a \
             newline/carriage-return character ({ch:?}) in the path — such a home or \
             claim path is never legitimate for server-run verification (#1454); fix the \
             TACHI_HOME/worktree path before running checks",
            copy_dir.display()
        ));
    }
    let _copy_guard = DetachedCopyGuard {
        runner,
        claim: worktree.clone(),
        copy: copy_dir.clone(),
    };
    runner.create_detached_copy(&worktree, &source_head, &copy_dir)?;

    // G4: tool version into the receipt (informational parity only).
    let tool_version = runner.tool_version(check_kind)?;

    // G2: the spawned argv carries a server-owned CARGO_TARGET_DIR so the
    // candidate config cannot redirect the build into the copy (which would
    // dirty it and forge a tree mutation). No other env scrubbing in this
    // leaf: the candidate process runs under the server UID regardless
    // (#1454 C1 named residual — OS-level isolation is #894's).
    let target_dir = tachi_home.join("verify-target");
    let env = [("CARGO_TARGET_DIR", target_dir.to_str().unwrap_or_default())];
    let outcome = runner
        .run_check(argv, &copy_dir, timeout, &log_path, &env)
        .await?;

    // #1454 G2: post-run integrity ON THE COPY — clean and still at
    // `source_head`. Anything else is a failed run with the typed reason
    // `tree_mutation_during_run` (never passed); the gate re-checks the
    // binding independently from the receipt fields.
    let copy_head_after = runner.observe_head(&copy_dir).await?;
    let copy_clean_after = runner.worktree_is_clean(&copy_dir).await?;
    let tree_mutated = copy_head_after != source_head || !copy_clean_after;

    let status = if tree_mutated {
        "failed"
    } else if outcome.exit_code == Some(0) {
        "passed"
    } else {
        "failed"
    };
    let reason = if tree_mutated {
        Some("tree_mutation_during_run")
    } else if outcome.timed_out {
        Some("timed_out")
    } else if status == "failed" {
        Some("failed")
    } else {
        None
    };
    let source = format!("{SERVER_RUN_SOURCE_PREFIX}{check_kind}");
    let ran_at = now();

    // Authority write FIRST: the receipt lands in the server-owned store
    // (`<tachi-home>/verify-receipts/<flow_id>/<kind>.json`), outside any
    // repository (F1). The ledger item below is the display echo.
    //
    // G2 receipt binding fields: `source_head` is the observed claim HEAD;
    // the copy is checked out AT `source_head` (so `copy_head_before` ==
    // `source_head` and `copy_clean_before` == true by construction of
    // `git worktree add` from a commit); `copy_head_after`/`copy_clean_after`
    // are the post-run observations ON THE COPY. `executed_in_detached_copy`
    // is the semantic marker the gate requires — a receipt without it binds
    // to nothing.
    let receipt = json!({
        "flow_id": flow_id,
        "kind": check_kind,
        "head_sha": source_head,
        "status": status,
        "reason": reason,
        "exit_code": outcome.exit_code,
        "log_path": log_path.display().to_string(),
        "duration_ms": outcome.duration_ms,
        "ran_at": ran_at,
        "timed_out": outcome.timed_out,
        "kill_abandoned": outcome.kill_abandoned,
        "source_head": source_head,
        "executed_in_detached_copy": true,
        "copy_head_before": source_head,
        "copy_head_after": copy_head_after,
        "copy_clean_before": true,
        "copy_clean_after": copy_clean_after,
        "tool_version": tool_version,
    });
    write_run_receipt(&tachi_home, flow_id, check_kind, &receipt)?;

    let item = json!({
        "id": check_kind,
        "check_id": check_kind,
        "kind": check_kind,
        "status": status,
        "required": true,
        "source": source,
        "head_sha": source_head,
        "exit_code": outcome.exit_code,
        "timed_out": outcome.timed_out,
        "kill_abandoned": outcome.kill_abandoned,
        "reason": reason,
        "log_path": log_path.display().to_string(),
        "duration_ms": outcome.duration_ms,
        "ran_at": ran_at,
        "updated_at": ran_at,
    });
    let mut raw = record_server_run_item(flow_id, item)?;
    raw["check_id"] = json!(check_kind);
    raw["item_status"] = json!(status);
    raw["head_sha"] = json!(source_head);
    raw["source"] = json!(source);
    raw["exit_code"] = outcome
        .exit_code
        .map(|code| json!(code))
        .unwrap_or(Value::Null);
    raw["timed_out"] = json!(outcome.timed_out);
    raw["kill_abandoned"] = json!(outcome.kill_abandoned);
    raw["reason"] = reason.map(|r| json!(r)).unwrap_or(Value::Null);
    raw["log_path"] = json!(log_path.display().to_string());
    raw["duration_ms"] = json!(outcome.duration_ms);
    raw["ran_at"] = json!(ran_at);
    // #1454 G3: the run response's top-level `overall` is the GATE overall
    // evaluated with the just-observed head; the raw ledger value moves to
    // `ledger_overall` (caller-asserted display detail, never authority).
    let gate = evaluate_verification_gate(Some(flow_id), &source_head, &tachi_home)?;
    raw["ledger_overall"] = raw["verification"]["overall"].clone();
    raw["overall"] = match &gate {
        Some(g) => g
            .get("overall")
            .cloned()
            .unwrap_or_else(|| json!("unverified")),
        None => json!("unverified"),
    };
    raw["gate"] = gate.unwrap_or_else(|| json!({ "overall": "unknown" }));
    Ok(raw)
}

#[cfg(test)]
mod tests;
