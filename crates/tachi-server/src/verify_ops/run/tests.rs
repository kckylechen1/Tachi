//! #1454 slice 2 unit tests: fake-runner path (hermetic, no real cargo).

use super::*;
use crate::tests::make_server;
use crate::tool_params::TachiTaskParams;
use crate::tool_params::TachiVerifyParams;
use crate::verify_ops::seed_run_receipt_for_test;

/// Fake runner: canned head + canned exit code + a canned log file. Proves
/// the run path end-to-end (claim resolution → head observation → detached
/// copy seam → spawn seam → receipt store → ledger write → gate) without
/// spawning a real check process. The detached-copy methods are filesystem
/// no-ops (mkdir / remove_dir_all) — the copy-lifecycle semantics against
/// real git are proven by the integration tests.
struct FakeRunner {
    exit_code: Option<i32>,
    head_sha: &'static str,
}

#[async_trait::async_trait]
impl CheckRunner for FakeRunner {
    async fn observe_head(&self, _worktree: &Path) -> Result<String, String> {
        Ok(self.head_sha.to_string())
    }

    async fn worktree_is_clean(&self, _worktree: &Path) -> Result<bool, String> {
        Ok(true)
    }

    async fn run_check(
        &self,
        _argv: &[&str],
        _cwd: &Path,
        _timeout: Duration,
        log_path: &Path,
        _env: &[(&str, &str)],
    ) -> Result<CheckRunOutcome, String> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(log_path, "canned fake run output\n").map_err(|e| e.to_string())?;
        Ok(CheckRunOutcome {
            exit_code: self.exit_code,
            duration_ms: 7,
            timed_out: false,
            kill_abandoned: false,
        })
    }

    fn create_detached_copy(
        &self,
        _claim: &Path,
        _observed_head: &str,
        dest: &Path,
    ) -> Result<(), String> {
        std::fs::create_dir_all(dest).map_err(|e| format!("fake copy mkdir: {e}"))
    }

    fn remove_detached_copy(&self, _claim: &Path, copy: &Path) -> Result<(), String> {
        std::fs::remove_dir_all(copy).map_err(|e| format!("fake copy remove: {e}"))
    }

    fn copy_is_registered(&self, _claim: &Path, copy: &Path) -> RegistrationProbe {
        // For the dir-backed fakes the "registration" IS the copy dir.
        if copy.exists() {
            RegistrationProbe::Registered
        } else {
            RegistrationProbe::NotRegistered
        }
    }

    fn tool_version(&self, _kind: &str) -> Result<Option<String>, String> {
        Ok(Some("fake-tool-1.0".to_string()))
    }
}

/// #1454 G2 discriminator fake: `run_check` mutates the DETACHED COPY (its
/// `cwd`) by physically creating a marker file and flags the post-run
/// `worktree_is_clean` observation to report newly-dirty — the copy-mutation
/// path that must record `failed` + `tree_mutation_during_run`.
struct MutatingRunner {
    head_sha: &'static str,
    mutated: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl CheckRunner for MutatingRunner {
    async fn observe_head(&self, _worktree: &Path) -> Result<String, String> {
        Ok(self.head_sha.to_string())
    }

    async fn worktree_is_clean(&self, _worktree: &Path) -> Result<bool, String> {
        Ok(!self.mutated.load(std::sync::atomic::Ordering::SeqCst))
    }

    async fn run_check(
        &self,
        _argv: &[&str],
        cwd: &Path,
        _timeout: Duration,
        log_path: &Path,
        _env: &[(&str, &str)],
    ) -> Result<CheckRunOutcome, String> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(log_path, "mutating fake run output\n").map_err(|e| e.to_string())?;
        // The "tree mutation mid-run": a caller-shaped process writes into the
        // DETACHED COPY while the check is supposedly running.
        std::fs::write(cwd.join("dirty-marker.txt"), "mutated during run\n")
            .map_err(|e| e.to_string())?;
        self.mutated
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(CheckRunOutcome {
            exit_code: Some(0),
            duration_ms: 7,
            timed_out: false,
            kill_abandoned: false,
        })
    }

    fn create_detached_copy(
        &self,
        _claim: &Path,
        _observed_head: &str,
        dest: &Path,
    ) -> Result<(), String> {
        std::fs::create_dir_all(dest).map_err(|e| format!("fake copy mkdir: {e}"))
    }

    fn remove_detached_copy(&self, _claim: &Path, copy: &Path) -> Result<(), String> {
        std::fs::remove_dir_all(copy).map_err(|e| format!("fake copy remove: {e}"))
    }

    fn copy_is_registered(&self, _claim: &Path, copy: &Path) -> RegistrationProbe {
        // For the dir-backed fakes the "registration" IS the copy dir.
        if copy.exists() {
            RegistrationProbe::Registered
        } else {
            RegistrationProbe::NotRegistered
        }
    }

    fn tool_version(&self, _kind: &str) -> Result<Option<String>, String> {
        Ok(Some("fake-tool-1.0".to_string()))
    }
}

/// #1454 G2 isolation discriminator fake: `run_check` mutates the CLAIM
/// worktree (a caller-shaped process writes into the claimed tree mid-run).
/// The post-run observations happen ON THE COPY, so this must NOT affect the
/// run — the detached copy isolates the executed tree from claim mutations.
struct ClaimMutatingRunner {
    head_sha: &'static str,
    claim: PathBuf,
}

#[async_trait::async_trait]
impl CheckRunner for ClaimMutatingRunner {
    async fn observe_head(&self, _worktree: &Path) -> Result<String, String> {
        Ok(self.head_sha.to_string())
    }

    async fn worktree_is_clean(&self, _worktree: &Path) -> Result<bool, String> {
        Ok(true)
    }

    async fn run_check(
        &self,
        _argv: &[&str],
        _cwd: &Path,
        _timeout: Duration,
        log_path: &Path,
        _env: &[(&str, &str)],
    ) -> Result<CheckRunOutcome, String> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(log_path, "claim-mutating fake run output\n").map_err(|e| e.to_string())?;
        // Mutate the CLAIM worktree mid-run — the copy must be unaffected.
        std::fs::write(
            self.claim.join("claim-dirtied-mid-run.txt"),
            "claim mutated during run\n",
        )
        .map_err(|e| e.to_string())?;
        Ok(CheckRunOutcome {
            exit_code: Some(0),
            duration_ms: 7,
            timed_out: false,
            kill_abandoned: false,
        })
    }

    fn create_detached_copy(
        &self,
        _claim: &Path,
        _observed_head: &str,
        dest: &Path,
    ) -> Result<(), String> {
        std::fs::create_dir_all(dest).map_err(|e| format!("fake copy mkdir: {e}"))
    }

    fn remove_detached_copy(&self, _claim: &Path, copy: &Path) -> Result<(), String> {
        std::fs::remove_dir_all(copy).map_err(|e| format!("fake copy remove: {e}"))
    }

    fn copy_is_registered(&self, _claim: &Path, copy: &Path) -> RegistrationProbe {
        // For the dir-backed fakes the "registration" IS the copy dir.
        if copy.exists() {
            RegistrationProbe::Registered
        } else {
            RegistrationProbe::NotRegistered
        }
    }

    fn tool_version(&self, _kind: &str) -> Result<Option<String>, String> {
        Ok(Some("fake-tool-1.0".to_string()))
    }
}

/// A child that ignores kills (#1454 F7): `start_kill` succeeds but `wait`
/// never resolves, so only the bounded post-kill wait can release the caller.
struct KillIgnoringChild {
    killed: bool,
}

#[async_trait::async_trait]
impl CheckChild for KillIgnoringChild {
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        std::future::pending::<()>().await;
        unreachable!("kill-ignoring child wait never resolves")
    }
    async fn start_kill(&mut self) -> std::io::Result<()> {
        self.killed = true;
        Ok(())
    }
}

/// #1454 F7 fake runner: run_check goes through the SAME bounded timeout
/// discipline as the real runner (`wait_with_kill_cap`) with a child that
/// ignores kills — the kill result does not gate the second cap.
struct KillIgnoringRunner;

#[async_trait::async_trait]
impl CheckRunner for KillIgnoringRunner {
    async fn observe_head(&self, _worktree: &Path) -> Result<String, String> {
        Ok("deadbeef0123456789abcdef0123456789abcdef01".to_string())
    }

    async fn worktree_is_clean(&self, _worktree: &Path) -> Result<bool, String> {
        Ok(true)
    }

    async fn run_check(
        &self,
        _argv: &[&str],
        _cwd: &Path,
        timeout: Duration,
        log_path: &Path,
        _env: &[(&str, &str)],
    ) -> Result<CheckRunOutcome, String> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(log_path, "hung fake run output\n").map_err(|e| e.to_string())?;
        let mut child = KillIgnoringChild { killed: false };
        wait_with_kill_cap(&mut child, timeout, POST_KILL_WAIT_CAP).await
    }

    fn create_detached_copy(
        &self,
        _claim: &Path,
        _observed_head: &str,
        dest: &Path,
    ) -> Result<(), String> {
        std::fs::create_dir_all(dest).map_err(|e| format!("fake copy mkdir: {e}"))
    }

    fn remove_detached_copy(&self, _claim: &Path, copy: &Path) -> Result<(), String> {
        std::fs::remove_dir_all(copy).map_err(|e| format!("fake copy remove: {e}"))
    }

    fn copy_is_registered(&self, _claim: &Path, copy: &Path) -> RegistrationProbe {
        // For the dir-backed fakes the "registration" IS the copy dir.
        if copy.exists() {
            RegistrationProbe::Registered
        } else {
            RegistrationProbe::NotRegistered
        }
    }

    fn tool_version(&self, _kind: &str) -> Result<Option<String>, String> {
        Ok(Some("fake-tool-1.0".to_string()))
    }
}

fn fake_runner() -> FakeRunner {
    FakeRunner {
        exit_code: Some(0),
        head_sha: "deadbeef0123456789abcdef0123456789abcdef01",
    }
}

/// A valid server-run receipt for `kind`/`head` — the shape the executor
/// writes (F1/G2). Tests seed the OTHER canonical kinds through the store so
/// the gate can reach `passed` for one real/fake run (prescribed for the
/// integration test; the same shape for unit runs).
fn valid_receipt(kind: &str, head: &str) -> Value {
    json!({
        "flow_id": "flow_seed",
        "kind": kind,
        "head_sha": head,
        "status": "passed",
        "reason": null,
        "exit_code": 0,
        "log_path": "/tmp/seed.log",
        "duration_ms": 1,
        "ran_at": "2026-08-18T00:00:00Z",
        "timed_out": false,
        "kill_abandoned": false,
        "source_head": head,
        "executed_in_detached_copy": true,
        "copy_head_before": head,
        "copy_head_after": head,
        "copy_clean_before": true,
        "copy_clean_after": true,
        "tool_version": "seed-tool-1.0",
    })
}

/// Seed every canonical kind EXCEPT `skip` (the one the run under test
/// produces itself) into the server's receipt store for `flow_id`/`head`.
fn seed_other_canonical_kinds(server: &MemoryServer, flow_id: &str, head: &str, skip: &str) {
    let home = server.tachi_home_dir();
    for kind in super::super::MERGE_REQUIRED_RUN_KINDS {
        if *kind == skip {
            continue;
        }
        let mut receipt = valid_receipt(kind, head);
        receipt["flow_id"] = json!(flow_id);
        seed_run_receipt_for_test(&home, flow_id, kind, &receipt).expect("seed receipt");
    }
}

fn claim_params(flow_id: &str, worktree: &str) -> TachiTaskParams {
    serde_json::from_value(serde_json::json!({
        "action": "claim",
        "flow_id": flow_id,
        "issue_ref": "org/repo#1454",
        "branch": "lane/1454-verify",
        "claim_role": "executor",
        "claim_mode": "writable",
        "worktree_path": worktree,
        "claim_scope": ["crates/tachi-server/src/verify_ops/run.rs"],
        "expected_head": "f7467b3",
        "lease_expires_at": "2030-01-01T00:00:00Z",
    }))
    .expect("claim params")
}

/// Admit a local agent and claim `flow_id` against `worktree` (follows the
/// claims_ops test fixture pattern, claims_ops.rs:958+).
fn seed_claim(server: &MemoryServer, flow_id: &str, worktree: &str) {
    crate::claims_ops::admit_agent_connection(server, Some("agent.alpha".to_string()), true)
        .expect("local admission");
    crate::claims_ops::handle_task_claim(server, &claim_params(flow_id, worktree))
        .expect("work claim");
}

struct RunRootGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    original: Option<std::ffi::OsString>,
}

impl Drop for RunRootGuard {
    fn drop(&mut self) {
        if let Some(original) = self.original.as_ref() {
            std::env::set_var("TACHI_RUN_ROOT", original);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }
}

fn with_run_root() -> (tempfile::TempDir, RunRootGuard) {
    // Lock FIRST: TACHI_RUN_ROOT is process-global, so no other test may be
    // reading/writing flow run dirs while this one owns the root.
    let lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("run root tempdir");
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    (
        tmp,
        RunRootGuard {
            _lock: lock,
            original,
        },
    )
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_exit_zero_writes_server_run_item_and_gate_passes() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-exit-zero";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    let runner = fake_runner();
    let raw = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect("run completes");

    // Receipt evidence — all server-observed.
    assert_eq!(raw["item_status"], "passed");
    assert_eq!(raw["head_sha"], runner.head_sha);
    assert_eq!(raw["source"], "server_run:fmt");
    assert_eq!(raw["check_id"], "fmt");
    assert_eq!(raw["exit_code"], 0);
    assert_eq!(raw["duration_ms"], 7);
    assert!(
        raw["log_path"]
            .as_str()
            .expect("log_path str")
            .contains("verify-run-fmt-"),
        "log path must name the run: {}",
        raw["log_path"]
    );
    // #1454 G3: the run response's top-level `overall` is the UNIFIED gate
    // verdict evaluated with the just-observed head — after only an fmt
    // receipt the gate is pending (missing canonical kinds), so `overall` is
    // `pending` even though the single item passed; the raw ledger value
    // moves to the `ledger_overall` detail.
    assert_eq!(raw["overall"], "pending");
    assert_eq!(raw["ledger_overall"], "passed");
    assert_eq!(raw["gate"]["overall"], "pending");
    assert!(raw["gate"]["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w == "verification:clippy:missing"));

    // #1454 F1: the AUTHORITY write is the receipt in the server-owned store
    // (outside any repository), not the ledger.
    let home = server.tachi_home_dir();
    let receipt_path = home.join("verify-receipts").join(flow_id).join("fmt.json");
    assert!(
        receipt_path.exists(),
        "receipt must land in the server store"
    );
    let receipt: Value =
        serde_json::from_str(&std::fs::read_to_string(&receipt_path).expect("read receipt"))
            .expect("receipt JSON");
    assert_eq!(receipt["kind"], "fmt");
    assert_eq!(receipt["status"], "passed");
    assert_eq!(receipt["head_sha"], runner.head_sha);
    // #1454 G2: detached-copy binding fields on every receipt — the copy is
    // checked out at the observed source head, executed in, and observed
    // clean+unmoved afterwards.
    assert_eq!(receipt["source_head"], runner.head_sha);
    assert_eq!(receipt["executed_in_detached_copy"], true);
    assert_eq!(receipt["copy_head_before"], runner.head_sha);
    assert_eq!(receipt["copy_head_after"], runner.head_sha);
    assert_eq!(receipt["copy_clean_before"], true);
    assert_eq!(receipt["copy_clean_after"], true);
    // #1454 G4: the kind's tool version is captured (informational).
    assert_eq!(receipt["tool_version"], "fake-tool-1.0");

    // Ledger item — display echo with every server_run field, nothing
    // caller-ish; `source` is display-only metadata (F1).
    let ledger = read_verification_ledger(flow_id).unwrap().unwrap();
    assert_eq!(ledger["overall"], "passed");
    let item = &ledger["items"][0];
    assert_eq!(item["id"], "fmt");
    assert_eq!(item["check_id"], "fmt");
    assert_eq!(item["kind"], "fmt");
    assert_eq!(item["status"], "passed");
    assert_eq!(item["source"], "server_run:fmt");
    assert_eq!(item["head_sha"], runner.head_sha);
    assert_eq!(item["required"], true);
    assert_eq!(item["exit_code"], 0);
    assert_eq!(item["duration_ms"], 7);
    assert!(item.get("ran_at").and_then(Value::as_str).is_some());
    let log_path = Path::new(item["log_path"].as_str().expect("log_path"));
    assert!(log_path.exists(), "run log must exist");
    assert!(
        std::fs::metadata(log_path).expect("log metadata").len() > 0,
        "run log must be non-empty"
    );

    // #1454 F4: the gate only reaches `passed` once the FULL canonical set
    // has valid receipts for the evaluated head. Seed the other six kinds
    // via the store test helper (the integration seam the dispatch allows),
    // then re-evaluate.
    seed_other_canonical_kinds(&server, flow_id, runner.head_sha, "fmt");
    let gate = crate::verify_ops::evaluate_verification_gate(Some(flow_id), runner.head_sha, &home)
        .unwrap()
        .unwrap();
    assert_eq!(gate["overall"], "passed");
    assert!(gate["passed"].as_array().unwrap().contains(&json!("fmt")));
    assert!(gate["waiting_on"].as_array().unwrap().is_empty());
    assert!(gate["reasons"].as_array().unwrap().is_empty());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_exit_nonzero_lands_in_failed_bucket() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-exit-one";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    let runner = FakeRunner {
        exit_code: Some(1),
        ..fake_runner()
    };
    let raw = run_with_runner(
        &server,
        &runner,
        flow_id,
        "clippy",
        check_kind_argv("clippy").expect("clippy argv"),
        Duration::from_secs(60),
    )
    .await
    .expect("run completes");

    assert_eq!(raw["item_status"], "failed");
    assert_eq!(raw["overall"], "failed");
    assert_eq!(raw["gate"]["overall"], "failed");
    assert!(raw["gate"]["failed"]
        .as_array()
        .unwrap()
        .contains(&json!("clippy")));

    let ledger = read_verification_ledger(flow_id).unwrap().unwrap();
    assert_eq!(ledger["overall"], "failed");
    assert_eq!(ledger["items"][0]["status"], "failed");
    assert_eq!(ledger["items"][0]["source"], "server_run:clippy");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_unknown_kind_is_typed_err_naming_the_closed_set() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let err = run_verification_check(&server, "flow_any", "bogus", None)
        .await
        .expect_err("unknown kind must be a typed Err");
    assert!(err.contains("unknown check_kind 'bogus'"), "{err}");
    assert!(
        err.contains("version-sync, clippy, fmt, audit, nextest, portable-contract, doc"),
        "{err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_claim_worktree_dirtiness_is_never_consulted() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-dirty-claim";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    // G2 re-anchor of the round-1 F3 `run_refuses_dirty_worktree_before_any_spawn`
    // guardian: the claim worktree is NEVER executed — the server runs a
    // detached copy at the observed HEAD — so the claim's own dirtiness is
    // no longer a refusal, and uncommitted content is still never run (the
    // copy binds `source_head`, a committed sha). Physically dirty the claim
    // with an untracked marker; the run must proceed and pass.
    std::fs::write(
        worktree.path().join("uncommitted-claim-marker.txt"),
        "dirty claim, but the claim is never executed\n",
    )
    .expect("dirty the claim");

    let runner = fake_runner();
    let raw = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect("dirty claim must not block the run");

    assert_eq!(raw["item_status"], "passed");
    assert_eq!(
        raw["head_sha"], runner.head_sha,
        "the run binds the observed committed HEAD, not the claim's uncommitted state"
    );

    // The receipt records the detached-copy execution — evidence for the
    // committed head only.
    let home = server.tachi_home_dir();
    let receipt: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join("verify-receipts").join(flow_id).join("fmt.json"))
            .expect("receipt"),
    )
    .expect("receipt JSON");
    assert_eq!(receipt["executed_in_detached_copy"], true);
    assert_eq!(receipt["source_head"], runner.head_sha);
    assert_eq!(receipt["status"], "passed");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_claim_worktree_equal_to_server_root_is_typed_err() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    // #1454 F5: a claim pointing AT the server's own project root (or an
    // ancestor) is rejected — the executor must not turn the serving tree
    // into a test target.
    let server_root = crate::path_utils::cached_git_root()
        .cloned()
        .expect("test process runs inside a git worktree");
    let flow_id = "flow_run-server-root";
    seed_claim(
        &server,
        flow_id,
        server_root.to_str().expect("utf8 server root"),
    );
    let runner = fake_runner();
    let err = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("server-root claim must be a typed Err");
    assert!(
        err.contains("server's own project root or an ancestor of it"),
        "{err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_tree_mutation_mid_run_records_failed_tree_mutation_during_run() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-tree-mutation";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    // #1454 G2 discriminator: exit code 0 but the DETACHED COPY is dirtied
    // mid-run — the run must be recorded `failed` with the typed reason
    // `tree_mutation_during_run` (never passed), and the gate must honor it.
    let runner = MutatingRunner {
        head_sha: "deadbeef0123456789abcdef0123456789abcdef01",
        mutated: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let raw = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect("run completes");
    assert_eq!(raw["item_status"], "failed");
    assert_eq!(raw["reason"], "tree_mutation_during_run");

    let home = server.tachi_home_dir();
    let receipt: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join("verify-receipts").join(flow_id).join("fmt.json"))
            .expect("receipt"),
    )
    .expect("receipt JSON");
    assert_eq!(receipt["status"], "failed");
    assert_eq!(receipt["reason"], "tree_mutation_during_run");
    assert_eq!(
        receipt["exit_code"], 0,
        "exit code is irrelevant once the copy tree moved"
    );
    assert_eq!(receipt["executed_in_detached_copy"], true);
    assert_eq!(receipt["copy_clean_before"], true);
    assert_eq!(receipt["copy_clean_after"], false);

    // The gate classifies the mutated-run receipt as failed — never passed.
    let gate = crate::verify_ops::evaluate_verification_gate(Some(flow_id), runner.head_sha, &home)
        .unwrap()
        .unwrap();
    assert_eq!(gate["overall"], "failed");
    assert!(gate["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:fmt:tree_mutation_during_run"));
    assert!(gate["passed"].as_array().unwrap().is_empty());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_claim_mutation_mid_run_does_not_affect_the_detached_copy() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-claim-mutation";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    // #1454 G2 discriminator (oracle M2): a caller-shaped process mutates the
    // CLAIM worktree while the check is running. The post-run observations
    // happen ON THE COPY, so the run must still pass — no pre/post snapshot
    // of the claim exists to TOCTOU.
    let runner = ClaimMutatingRunner {
        head_sha: "deadbeef0123456789abcdef0123456789abcdef01",
        claim: worktree.path().to_path_buf(),
    };
    let raw = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect("run completes");
    assert_eq!(raw["item_status"], "passed");
    assert_eq!(raw["reason"], Value::Null);
    assert!(
        worktree.path().join("claim-dirtied-mid-run.txt").exists(),
        "the claim was physically mutated mid-run (the discriminator's premise)"
    );

    let home = server.tachi_home_dir();
    let receipt: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join("verify-receipts").join(flow_id).join("fmt.json"))
            .expect("receipt"),
    )
    .expect("receipt JSON");
    assert_eq!(receipt["status"], "passed");
    assert_eq!(receipt["executed_in_detached_copy"], true);
    assert_eq!(receipt["copy_clean_after"], true);
    assert_eq!(
        receipt["copy_head_after"], runner.head_sha,
        "the copy must still be at the observed head"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_canonical_set_completeness_single_kind_pending_naming_missing_kinds() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-canonical-set";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    // #1454 F4 discriminator (e): a receipt for only {fmt} must leave the
    // gate pending, naming every missing canonical kind.
    let runner = fake_runner();
    run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect("run completes");

    let home = server.tachi_home_dir();
    let gate = crate::verify_ops::evaluate_verification_gate(Some(flow_id), runner.head_sha, &home)
        .unwrap()
        .unwrap();
    assert_eq!(gate["overall"], "pending");
    assert_eq!(gate["passed"], json!(["fmt"]));
    let waiting: Vec<String> = gate["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    for missing in [
        "version-sync",
        "clippy",
        "audit",
        "nextest",
        "portable-contract",
        "doc",
    ] {
        assert!(
            waiting.contains(&format!("verification:{missing}:missing")),
            "gate must name {missing} as missing: {waiting:?}"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn timeout_ignored_kill_is_bounded_and_guard_released() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-kill-ignored";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    // #1454 F7: the child ignores the kill; the post-kill wait is bounded by
    // POST_KILL_WAIT_CAP, the run records timed_out + kill_abandoned, and
    // the per-flow in-flight guard is released (RAII) so a next run for the
    // same flow is not wedged.
    let runner = KillIgnoringRunner;
    let timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let raw = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        timeout,
    )
    .await
    .expect("run returns (bounded)");
    let elapsed = started.elapsed();

    assert_eq!(raw["item_status"], "failed");
    assert_eq!(raw["timed_out"], true);
    assert_eq!(raw["kill_abandoned"], true);
    assert_eq!(raw["reason"], "timed_out");

    // Call returns within (timeout + second cap + slack); never unbounded.
    let budget = timeout + POST_KILL_WAIT_CAP + Duration::from_secs(2);
    assert!(
        elapsed <= budget,
        "run must return within {budget:?}, took {elapsed:?}"
    );
    assert!(
        elapsed >= timeout,
        "run must not return before the timeout fires"
    );

    // The in-flight guard was released via RAII: a second run acquires it.
    let _reacquired = acquire_flow_run_guard(flow_id).expect("guard released after abandon");
    drop(_reacquired);

    // The receipt records the abandonment for the gate/audit trail.
    let home = server.tachi_home_dir();
    let receipt: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join("verify-receipts").join(flow_id).join("fmt.json"))
            .expect("receipt"),
    )
    .expect("receipt JSON");
    assert_eq!(receipt["timed_out"], true);
    assert_eq!(receipt["kill_abandoned"], true);
    assert_eq!(receipt["status"], "failed");
    assert_eq!(receipt["reason"], "timed_out");

    // #1454 G2 cleanup on the timeout path: the detached copy is removed on
    // ALL exits via the RAII guard — nothing remains under verify-worktrees.
    let worktrees_root = home.join("verify-worktrees");
    let leftovers: Vec<_> = std::fs::read_dir(&worktrees_root)
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "no detached copy may survive a timed-out run: {leftovers:?}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_without_active_claim_is_typed_err() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    // No claim at all → fail-closed before any process spawn.
    let runner = fake_runner();
    let err = run_with_runner(
        &server,
        &runner,
        "flow_no-claim",
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("no claim must be a typed Err");
    assert!(
        err.contains("requires an active claim with a worktree for flow flow_no-claim"),
        "{err}"
    );

    // A claim without an ACTIVE state is equally fail-closed: the executor
    // must not run against a non-active claim's tree.
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_orphaned";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );
    server
        .with_global_store(|store| {
            store
                .connection_mut()
                .execute(
                    "UPDATE session_claims SET state='orphaned' WHERE flow_id=?1",
                    [&flow_id],
                )
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("orphan claim");
    let err = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("non-active claim must be a typed Err");
    assert!(
        err.contains("requires an active claim with a worktree for flow flow_orphaned"),
        "{err}"
    );

    // An ACTIVE claim without a worktree path is equally fail-closed. The
    // claim layer only REQUIRES a worktree for writable claims (memcore:
    // "writable WorkClaim worktree path is required"), so a read_only claim
    // legitimately has none — and the executor must refuse to run in it.
    let no_tree_flow = "flow_no-worktree-field";
    let params: TachiTaskParams = serde_json::from_value(serde_json::json!({
        "action": "claim",
        "flow_id": no_tree_flow,
        "issue_ref": "org/repo#1454",
        "branch": "lane/1454-verify",
        "claim_role": "executor",
        "claim_mode": "read_only",
        "claim_scope": ["crates/tachi-server/src/verify_ops/run.rs"],
        "expected_head": "f7467b3",
        "lease_expires_at": "2030-01-01T00:00:00Z",
    }))
    .expect("read_only claim params");
    crate::claims_ops::handle_task_claim(&server, &params).expect("claim without worktree");
    let err = run_with_runner(
        &server,
        &runner,
        no_tree_flow,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("claim without worktree must be a typed Err");
    assert!(
        err.contains("requires an active claim with a worktree for flow flow_no-worktree-field"),
        "{err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn concurrent_second_run_for_same_flow_is_typed_err() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-concurrent";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    // Hold the in-flight marker, then a second run for the same flow must be
    // refused before any spawn.
    let _held = acquire_flow_run_guard(flow_id).expect("first acquisition");
    let runner = fake_runner();
    let err = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("second concurrent run must be refused");
    assert!(
        err.contains("already in progress for flow flow_run-concurrent"),
        "{err}"
    );

    // After release the same flow runs again — the guard is not sticky.
    drop(_held);
    run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect("run succeeds once the in-flight marker is released");
}

/// #1454 discriminator: caller-authored `head_sha`/`status` on a run request
/// are ignored — the wire accepts them (they are optional params on
/// `TachiVerifyParams`), but the executor never reads them: the written item
/// carries the server-observed head and the exit-code-derived status.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn caller_head_sha_and_status_are_ignored_server_observed_values_win() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-caller-ignored";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    // The wire carries caller-asserted head_sha/status alongside action=run;
    // they deserialize fine (run does not reject them — it ignores them).
    let params: TachiVerifyParams = serde_json::from_value(serde_json::json!({
        "action": "run",
        "format": "json",
        "flow_id": flow_id,
        "check_kind": "fmt",
        "head_sha": "caller-fake-head",
        "status": "failed",
        "timeout_secs": "30",
    }))
    .expect("run params deserialize");
    assert_eq!(params.head_sha.as_deref(), Some("caller-fake-head"));
    assert_eq!(params.status.as_deref(), Some("failed"));
    assert_eq!(params.timeout_secs, Some(30));

    // The handler-shaped extraction passes ONLY flow_id/check_kind/timeout to
    // the executor — head_sha/status never reach it.
    let runner = fake_runner();
    let raw = run_with_runner(
        &server,
        &runner,
        params.flow_id.as_deref().expect("flow_id"),
        params.check_kind.as_deref().expect("check_kind"),
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(
            params
                .timeout_secs
                .unwrap_or(DEFAULT_RUN_TIMEOUT_SECS)
                .min(MAX_RUN_TIMEOUT_SECS),
        ),
    )
    .await
    .expect("run completes");

    assert_eq!(
        raw["head_sha"], "deadbeef0123456789abcdef0123456789abcdef01",
        "server-observed head must win over the caller's 'caller-fake-head'"
    );
    assert_eq!(
        raw["item_status"], "passed",
        "exit-code-derived status must win"
    );
    assert_eq!(raw["source"], "server_run:fmt");

    let ledger = read_verification_ledger(flow_id).unwrap().unwrap();
    let item = &ledger["items"][0];
    assert_eq!(item["head_sha"], runner.head_sha);
    assert_eq!(item["status"], "passed");
    assert_eq!(item["source"], "server_run:fmt");
    assert!(
        item.get("summary").is_none(),
        "no caller summary may leak into a server_run item: {item}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn timeout_is_capped_at_3600_seconds() {
    let (_root, _guard) = with_run_root();
    // The cap is applied before any runner is consulted — verify the mapping
    // used by run_verification_check directly.
    let capped = Duration::from_secs(999_999u64.min(MAX_RUN_TIMEOUT_SECS));
    assert_eq!(capped, Duration::from_secs(MAX_RUN_TIMEOUT_SECS));
    let default = Duration::from_secs(DEFAULT_RUN_TIMEOUT_SECS);
    assert_eq!(default.as_secs(), 1800);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_receipts_root_inside_git_repo_is_typed_refusal() {
    let (_root, _guard) = with_run_root();
    // #1454 G1: the server-owned receipt store must not resolve inside a git
    // worktree. Point the server's Tachi home INTO a git repo: the executor
    // must refuse loudly with the typed message before any check runs.
    let repo = tempfile::tempdir().expect("git repo tempdir");
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .expect("git init");
    assert!(init.success(), "git init failed");
    let inside = repo.path().join("tachi-home-inside-repo");
    std::fs::create_dir_all(&inside).expect("create home inside repo");

    let server =
        MemoryServer::new_with_home_for_test(inside.join("global.db"), None, inside.clone())
            .expect("test server at a home inside a git repo");
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-home-in-repo";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    let runner = fake_runner();
    let err = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("receipts root inside a git worktree must be a typed refusal");
    assert!(err.contains("resolves inside a git worktree"), "{err}");
    assert!(err.contains("authorable location"), "{err}");
    assert!(
        !inside
            .join("verify-receipts")
            .join(flow_id)
            .join("fmt.json")
            .exists(),
        "no receipt may be written when the G1 guard refuses"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_captures_tool_version_per_kind_into_receipt() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-tool-version";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    // #1454 G4: before running a kind, the executor captures the tool's
    // version into the receipt (informational parity — no gate comparison).
    let runner = fake_runner();
    let raw = run_with_runner(
        &server,
        &runner,
        flow_id,
        "audit",
        check_kind_argv("audit").expect("audit argv"),
        Duration::from_secs(60),
    )
    .await
    .expect("run completes");
    assert_eq!(raw["item_status"], "passed");

    let home = server.tachi_home_dir();
    let receipt: Value = serde_json::from_str(
        &std::fs::read_to_string(
            home.join("verify-receipts")
                .join(flow_id)
                .join("audit.json"),
        )
        .expect("receipt"),
    )
    .expect("receipt JSON");
    assert_eq!(
        receipt["tool_version"], "fake-tool-1.0",
        "the fake runner's canned tool version must land in the receipt"
    );
}

// ─── Integration: real ProcessCheckRunner + real git (linked worktrees) ────

/// Build a MAIN repo with one committed file plus a LINKED worktree
/// (`git worktree add`) to serve as the claim worktree. This is the
/// production shape a claimed worktree has, and it is what G2 must
/// empirically prove `git worktree add` works FROM (a linked worktree).
/// Returns (main-repo tempdir, claim worktree path, committed head sha).
fn linked_claim_fixture(canonical: bool) -> (tempfile::TempDir, PathBuf, String) {
    let main = tempfile::tempdir().expect("main repo tempdir");
    let git = |args: &[&str], cwd: &std::path::Path| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?} in {} failed: {e}", cwd.display()));
        assert!(status.success(), "git {args:?} in {} failed", cwd.display());
    };
    git(&["init", "-q"], main.path());
    for (key, value) in [
        ("user.email", "verify-test@example.com"),
        ("user.name", "verify-test"),
    ] {
        git(&["config", key, value], main.path());
    }
    std::fs::write(
        main.path().join("Cargo.toml"),
        "[package]\nname = \"verify-fixture-crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::create_dir_all(main.path().join("src")).expect("create src");
    // rustfmt-canonical vs non-canonical content decides whether a real
    // `cargo fmt --all --check` in the detached copy exits 0 or 1.
    let lib = if canonical {
        "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n"
    } else {
        "pub fn  add(a : u32,b : u32) -> u32 { return a + b; }\n"
    };
    std::fs::write(main.path().join("src").join("lib.rs"), lib).expect("write lib.rs");
    std::fs::write(main.path().join(".gitignore"), "/target\n").expect("write .gitignore");
    git(&["add", "-A"], main.path());
    git(&["commit", "-q", "-m", "fixture init"], main.path());
    // The LINKED worktree the claim will point at.
    let claim = main.path().join("claim-wt");
    git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane/claim",
            claim.to_str().unwrap(),
        ],
        main.path(),
    );
    let head_output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&claim)
        .output()
        .expect("git rev-parse");
    let head = String::from_utf8_lossy(&head_output.stdout)
        .trim()
        .to_string();
    assert_eq!(head.len(), 40, "fixture HEAD must be a full sha");
    (main, claim, head)
}

/// Paths of every registered worktree of the repo containing `claim`.
/// #1454 P1: parses `--porcelain -z` (NUL-terminated fields, records begin
/// `worktree <path>`) so a newline-bearing path cannot split a record — the
/// test-side mirrors the repaired production parser instead of replicating
/// the `.lines()` blind spot it replaced.
fn registered_worktree_paths(claim: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain", "-z"])
        .current_dir(claim)
        .output()
        .expect("git worktree list");
    assert!(output.status.success(), "git worktree list failed");
    let text = String::from_utf8_lossy(&output.stdout);
    text.split('\0')
        .filter_map(|field| field.strip_prefix("worktree "))
        .map(str::to_string)
        .collect()
}

/// #1454 slice 2 + G2 integration: `check_kind="fmt"` against a temp fixture
/// crate served through a LINKED worktree claim. Goes through the HANDLER
/// (`action='run'`) end-to-end: claim → server-observed HEAD → server-owned
/// detached copy created FROM the linked worktree → real cargo fmt spawn in
/// the copy → ledger write → receipt.
///
/// This is the empirical proof that `git worktree add` works from a linked
/// worktree (the executor's G2 copy step), that the copy is what gets
/// executed, and that the copy is deregistered on success.
///
/// rustfmt presence is checked FIRST: a sandbox without rustfmt makes the
/// real-runner path unexercisable, and this test must say so loudly rather
/// than silently skip — the failing branch is reported in the panic.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn integration_fmt_run_uses_detached_copy_from_linked_worktree_and_cleans_up() {
    let fmt_available = std::process::Command::new("cargo")
        .args(["fmt", "--version"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    assert!(
        fmt_available,
        "integration branch REFUSED: sandbox lacks rustfmt (`cargo fmt --version` failed); \
         the real-runner check cannot be exercised here — REFUSING to silently skip"
    );

    let (_root, _guard) = with_run_root();
    let server = make_server();
    let (_main, claim, expected_head) = linked_claim_fixture(true);
    let flow_id = "flow_integration-linked-fmt";
    seed_claim(&server, flow_id, claim.to_str().expect("utf8 worktree"));

    let params: TachiVerifyParams = serde_json::from_value(serde_json::json!({
        "action": "run",
        "format": "json",
        "flow_id": flow_id,
        "check_kind": "fmt",
    }))
    .expect("run params");
    let resp = handle_tachi_verify(&server, params)
        .await
        .expect("handler run completes");
    let value: Value = serde_json::from_str(&resp).expect("receipt JSON");

    assert_eq!(value["ok"], true);
    assert_eq!(value["check_id"], "fmt");
    assert_eq!(value["status"], "passed");
    assert_eq!(
        value["head_sha"], expected_head,
        "server-observed HEAD must equal the linked-worktree commit sha"
    );
    assert_eq!(value["source"], "server_run:fmt");
    assert_eq!(value["exit_code"], 0);
    // #1454 G3 unified verdict: only an fmt receipt exists, so the gate is
    // pending; the raw ledger value is a detail row.
    assert_eq!(value["overall"], "pending");
    assert_eq!(value["ledger_overall"], "passed");

    let ledger = read_verification_ledger(flow_id).unwrap().unwrap();
    assert_eq!(ledger["overall"], "passed");
    let item = &ledger["items"][0];
    assert_eq!(item["head_sha"], expected_head);
    assert_eq!(item["source"], "server_run:fmt");
    assert_eq!(item["status"], "passed");
    assert_eq!(item["exit_code"], 0);
    let log_path = Path::new(item["log_path"].as_str().expect("log_path"));
    assert!(log_path.exists(), "run log must exist");
    assert!(
        std::fs::metadata(log_path).expect("log metadata").len() > 0,
        "run log must be non-empty"
    );

    // #1454 F1/G2: the authority write is the receipt in the server-owned
    // store, carrying the detached-copy binding fields (source_head,
    // executed_in_detached_copy, copy clean/head, tool_version).
    let home = server.tachi_home_dir();
    let receipt_path = home.join("verify-receipts").join(flow_id).join("fmt.json");
    assert!(
        receipt_path.exists(),
        "fmt receipt must land in the server store"
    );
    let receipt: Value =
        serde_json::from_str(&std::fs::read_to_string(&receipt_path).expect("read receipt"))
            .expect("receipt JSON");
    assert_eq!(receipt["kind"], "fmt");
    assert_eq!(receipt["head_sha"], expected_head);
    assert_eq!(receipt["source_head"], expected_head);
    assert_eq!(receipt["executed_in_detached_copy"], true);
    assert_eq!(receipt["copy_head_before"], expected_head);
    assert_eq!(receipt["copy_head_after"], expected_head);
    assert_eq!(receipt["copy_clean_before"], true);
    assert_eq!(receipt["copy_clean_after"], true);
    let tool_version = receipt["tool_version"].as_str().expect("tool_version str");
    assert!(
        tool_version.contains("cargo"),
        "cargo kind tool version must be captured: {tool_version}"
    );

    // #1454 G2 cleanup on success: no detached copy remains registered in
    // the claim repo's worktree list.
    let registered = registered_worktree_paths(&claim);
    assert!(
        !registered
            .iter()
            .any(|path| path.contains("verify-worktrees")),
        "detached copy must be deregistered after a successful run: {registered:?}"
    );
    let worktrees_root = home.join("verify-worktrees");
    let leftovers: Vec<_> = std::fs::read_dir(&worktrees_root)
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "no detached copy dir may survive a successful run: {leftovers:?}"
    );

    // #1454 F1/F4: the gate passes once the FULL canonical set is present.
    // The other six kinds are seeded via the store test helper (the
    // dispatch-allowed integration seam); the fmt kind is the REAL run.
    seed_other_canonical_kinds(&server, flow_id, &expected_head, "fmt");
    let gate = crate::verify_ops::evaluate_verification_gate(Some(flow_id), &expected_head, &home)
        .unwrap()
        .unwrap();
    assert_eq!(
        gate["overall"], "passed",
        "gate must pass with the full canonical set"
    );
    assert!(gate["passed"].as_array().unwrap().contains(&json!("fmt")));
    assert!(gate["waiting_on"].as_array().unwrap().is_empty());
}

/// #1454 G2 cleanup on the FAILURE path, against real git: a committed
/// non-canonical fixture makes the real `cargo fmt --all --check` exit 1; the
/// run records `failed`, and the detached copy is deregistered on exit.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn integration_fmt_run_failure_cleans_up_detached_copy() {
    let fmt_available = std::process::Command::new("cargo")
        .args(["fmt", "--version"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    assert!(
        fmt_available,
        "integration branch REFUSED: sandbox lacks rustfmt (`cargo fmt --version` failed)"
    );

    let (_root, _guard) = with_run_root();
    let server = make_server();
    let (_main, claim, expected_head) = linked_claim_fixture(false);
    let flow_id = "flow_integration-linked-fmt-fail";
    seed_claim(&server, flow_id, claim.to_str().expect("utf8 worktree"));

    let params: TachiVerifyParams = serde_json::from_value(serde_json::json!({
        "action": "run",
        "format": "json",
        "flow_id": flow_id,
        "check_kind": "fmt",
    }))
    .expect("run params");
    let resp = handle_tachi_verify(&server, params)
        .await
        .expect("handler run completes");
    let value: Value = serde_json::from_str(&resp).expect("receipt JSON");

    assert_eq!(value["status"], "failed");
    assert_eq!(value["exit_code"], 1);
    assert_eq!(value["head_sha"], expected_head);

    let home = server.tachi_home_dir();
    let receipt: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join("verify-receipts").join(flow_id).join("fmt.json"))
            .expect("receipt"),
    )
    .expect("receipt JSON");
    assert_eq!(receipt["status"], "failed");
    assert_eq!(receipt["reason"], "failed");
    assert_eq!(receipt["source_head"], expected_head);
    assert_eq!(receipt["executed_in_detached_copy"], true);

    // Cleanup on failure: no copy registered, no dir left behind.
    let registered = registered_worktree_paths(&claim);
    assert!(
        !registered
            .iter()
            .any(|path| path.contains("verify-worktrees")),
        "detached copy must be deregistered after a failed run: {registered:?}"
    );
    let leftovers: Vec<_> = std::fs::read_dir(home.join("verify-worktrees"))
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "no detached copy dir may survive a failed run: {leftovers:?}"
    );
}

/// #1454 H1 (oracle major 1): a `post-checkout` hook exiting nonzero makes
/// `git worktree add` fail AFTER the copy dir and registration were created
/// (verified empirically: git propagates rc=42, the copy stays registered in
/// `worktree list --porcelain`, and the dir survives). The run must error
/// TYPED and still clean both up — the RAII guard must cover PARTIAL
/// creation, not just a fully-created copy.
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn integration_partial_worktree_add_failure_cleans_up_copy_and_registration() {
    use std::os::unix::fs::PermissionsExt;

    let (_main, claim, _expected_head) = linked_claim_fixture(true);
    // Linked worktrees resolve hooks to the COMMON git dir, so the failing
    // hook lives in the main repo's hooks dir and fires when the detached
    // copy is checked out.
    let hook = _main
        .path()
        .join(".git")
        .join("hooks")
        .join("post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nexit 42\n").expect("write failing hook");
    let mut perms = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&hook, perms).expect("chmod hook");

    let (_root, _guard) = with_run_root();
    let server = make_server();
    let flow_id = "flow_integration-partial-add";
    seed_claim(&server, flow_id, claim.to_str().expect("utf8 worktree"));

    // Real git copy lifecycle: the failing hook only matters against a real
    // `git worktree add`.
    let runner = RealCopyKillIgnoringRunner {
        claim: claim.clone(),
    };
    let err = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("partial worktree add must be a typed run error");
    assert!(
        err.contains("Preparing worktree") || err.contains("git worktree add"),
        "typed error must carry the git add failure: {err}"
    );

    // H1: neither the registration nor the dir may survive the failed add.
    let registered = registered_worktree_paths(&claim);
    assert!(
        !registered
            .iter()
            .any(|path| path.contains("verify-worktrees")),
        "partial-add copy must be deregistered: {registered:?}"
    );
    let home = server.tachi_home_dir();
    let leftovers: Vec<_> = std::fs::read_dir(home.join("verify-worktrees"))
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "partial-add copy dir must be removed: {leftovers:?}"
    );
}

/// #1454 H3 (oracle minor): the G1 walk-up `.git` fallback must run whenever
/// git is unavailable OR returns nonzero/unparseable — only a VERIFIABLE git
/// `false` (exit 0) short-circuits to allowed. Mechanism (stated): a
/// linked-worktree-style `.git` FILE whose `gitdir:` target does not exist
/// makes real git exit nonzero (`fatal: not a git repository`, rc=128 —
/// asserted below), so the fallback is the only honest answer source, and it
/// finds the `.git` file → the guard must refuse.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_receipts_root_with_broken_git_metadata_still_refuses_via_walk_up_fallback() {
    let (_root, _guard) = with_run_root();
    let home = tempfile::tempdir().expect("home tempdir");
    std::fs::write(
        home.path().join(".git"),
        format!("gitdir: {}\n", home.path().join("missing-gitdir").display()),
    )
    .expect("write dangling .git file");

    // Premise: real git returns NONZERO inside this dir (broken repo
    // metadata) — the old fail-open returned `false` here without ever
    // consulting the walk-up fallback.
    let probe = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(home.path())
        .output()
        .expect("git probe spawn");
    assert!(
        !probe.status.success(),
        "premise broken: git succeeded on a dangling gitdir file ({})",
        String::from_utf8_lossy(&probe.stderr).trim()
    );

    let server = MemoryServer::new_with_home_for_test(
        home.path().join("global.db"),
        None,
        home.path().to_path_buf(),
    )
    .expect("test server at a home with broken git metadata");
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-dangling-gitdir";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    let runner = fake_runner();
    let err = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("broken git metadata must not fail open: the walk-up fallback refuses");
    assert!(err.contains("resolves inside a git worktree"), "{err}");
    assert!(err.contains("authorable location"), "{err}");
}

/// #1454 G2/G7 cleanup on the TIMEOUT path against real git: the copy
/// lifecycle uses real `git worktree add/remove`, while `run_check` uses the
/// kill-ignoring child — the bounded post-kill wait fires, the run records
/// `timed_out` + `kill_abandoned`, and the RAII guard STILL deregisters the
/// copy.
struct RealCopyKillIgnoringRunner {
    claim: PathBuf,
}

#[async_trait::async_trait]
impl CheckRunner for RealCopyKillIgnoringRunner {
    async fn observe_head(&self, worktree: &Path) -> Result<String, String> {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(worktree)
            .output()
            .map_err(|e| format!("git rev-parse spawn: {e}"))?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn worktree_is_clean(&self, worktree: &Path) -> Result<bool, String> {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(worktree)
            .output()
            .map_err(|e| format!("git status spawn: {e}"))?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().is_empty())
    }

    async fn run_check(
        &self,
        _argv: &[&str],
        _cwd: &Path,
        timeout: Duration,
        log_path: &Path,
        _env: &[(&str, &str)],
    ) -> Result<CheckRunOutcome, String> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(log_path, "hung real-copy run output\n").map_err(|e| e.to_string())?;
        let mut child = KillIgnoringChild { killed: false };
        wait_with_kill_cap(&mut child, timeout, POST_KILL_WAIT_CAP).await
    }

    fn create_detached_copy(
        &self,
        _claim: &Path,
        observed_head: &str,
        dest: &Path,
    ) -> Result<(), String> {
        let output = std::process::Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(dest)
            .arg(observed_head)
            .current_dir(&self.claim)
            .output()
            .map_err(|e| format!("git worktree add spawn: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    fn remove_detached_copy(&self, _claim: &Path, copy: &Path) -> Result<(), String> {
        let output = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(copy)
            .current_dir(&self.claim)
            .output()
            .map_err(|e| format!("git worktree remove spawn: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    fn copy_is_registered(&self, claim: &Path, copy: &Path) -> RegistrationProbe {
        let output = match std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain", "-z"])
            .current_dir(claim)
            .output()
        {
            Ok(output) => output,
            Err(err) => {
                return RegistrationProbe::Unknown(format!(
                    "git worktree list in {} failed to spawn: {err}",
                    claim.display()
                ));
            }
        };
        if !output.status.success() {
            return RegistrationProbe::Unknown(format!(
                "git worktree list in {} exited nonzero",
                claim.display()
            ));
        }
        // #1454 O1: canonicalize BOTH sides (best-effort, missing leaf falls
        // back to the nearest existing ancestor + suffix) and match if ANY of
        // {raw-equal, canon-equal} holds in either direction — git emits
        // canonical spellings (`/private/var/...`), `copy` is uncanonical.
        // #1454 P1: `--porcelain -z` keeps a newline-bearing path inside its
        // own NUL-terminated `worktree <path>` field (mirrors the repaired
        // production parser).
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

    fn tool_version(&self, _kind: &str) -> Result<Option<String>, String> {
        Ok(Some("fake-tool-1.0".to_string()))
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn integration_timeout_cleans_up_detached_copy_in_claim_repo() {
    let (_root, _guard) = with_run_root();
    let server = make_server();
    let (_main, claim, _expected_head) = linked_claim_fixture(true);
    let flow_id = "flow_integration-timeout-copy";
    seed_claim(&server, flow_id, claim.to_str().expect("utf8 worktree"));

    let runner = RealCopyKillIgnoringRunner {
        claim: claim.clone(),
    };
    let timeout = Duration::from_millis(300);
    let raw = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        timeout,
    )
    .await
    .expect("run returns (bounded)");

    assert_eq!(raw["item_status"], "failed");
    assert_eq!(raw["timed_out"], true);
    assert_eq!(raw["kill_abandoned"], true);

    // Cleanup on the timeout path against real git: the copy is deregistered
    // and the dir removed even though the child was abandoned.
    let registered = registered_worktree_paths(&claim);
    assert!(
        !registered
            .iter()
            .any(|path| path.contains("verify-worktrees")),
        "detached copy must be deregistered after a timed-out run: {registered:?}"
    );
    let home = server.tachi_home_dir();
    let leftovers: Vec<_> = std::fs::read_dir(home.join("verify-worktrees"))
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "no detached copy dir may survive a timed-out run: {leftovers:?}"
    );
}

/// #1454 O1 (oracle major): the registration comparison must survive the
/// `/var` ↔ `/private/var` aliasing (git emits the canonical spelling, the
/// construction is uncanonical) — INCLUDING the registration-only-residue
/// case where the copy dir no longer exists and a plain `canonicalize()`
/// fails on both sides. Portable variant: construct the copy under a path
/// whose canonical form differs from the construction via a SYMLINKED
/// PARENT (the repair instruction's stated fallback when the environment
/// cannot produce the macOS alias — the comparison function is exercised
/// with both spellings here, on any unix host).
#[cfg(unix)]
#[test]
fn registration_comparison_survives_symlink_alias_including_gone_dir_residue() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("real");
    std::fs::create_dir_all(real.join("verify-worktrees")).expect("real verify-worktrees");
    let alias = tmp.path().join("alias");
    symlink(&real, &alias).expect("symlink alias parent");

    // Construction through the alias (uncanonic spelling — like a
    // `tachi_home` rooted at /var); the registered path is the CANONICAL
    // spelling (like git's /private/var output).
    let construction = alias.join("verify-worktrees").join("flow-fmt-1");
    let registered = real.join("verify-worktrees").join("flow-fmt-1");
    assert_ne!(
        construction, registered,
        "premise: the aliased construction must differ from the canonical spelling"
    );

    // Dir EXISTS: canonicalize resolves the alias → canon-equal.
    std::fs::create_dir_all(&construction).expect("create copy via alias");
    assert!(
        worktree_paths_match(
            &registered,
            &construction,
            canonical_path_best_effort(&registered),
            canonical_path_best_effort(&construction).as_deref(),
        ),
        "dir-present copy must match across the alias spelling"
    );

    // Registration-only residue: the dir is GONE, the registration persists.
    // A plain canonicalize fails on both sides; the best-effort ancestor
    // resolution must still converge them.
    std::fs::remove_dir_all(&registered).expect("remove copy dir");
    assert!(
        worktree_paths_match(
            &registered,
            &construction,
            canonical_path_best_effort(&registered),
            canonical_path_best_effort(&construction).as_deref(),
        ),
        "gone-dir registration must still match across the alias spelling"
    );

    // A genuinely different registration must NOT match.
    let other = real.join("verify-worktrees").join("flow-other-2");
    assert!(
        !worktree_paths_match(
            &other,
            &construction,
            canonical_path_best_effort(&other),
            canonical_path_best_effort(&construction).as_deref(),
        ),
        "a different path must not match"
    );
}

/// #1454 O1 (oracle major) — macOS variant against REAL git: the tempdir
/// alias (on macOS `$TMPDIR` is `/var/folders/...` while the canonical
/// spelling is `/private/var/folders/...`) makes the construction and git's
/// registered spelling differ exactly like the oracle's case. The copy is
/// registered by real `git worktree add`, then its dir is removed — the
/// registration-only residue must still be reported Registered.
#[cfg(target_os = "macos")]
#[test]
fn copy_is_registered_detects_gone_dir_residue_across_var_private_var_alias() {
    let (_main, claim, head) = linked_claim_fixture(true);
    let tmp = tempfile::tempdir().expect("tempdir under aliased TMPDIR");
    let copy = tmp.path().join("verify-worktrees").join("flow-fmt-1");
    std::fs::create_dir_all(&copy).expect("create copy dir");

    // Premise: on macOS the raw construction spelling is NOT canonical
    // (`/var/...` vs `/private/var/...`) — the exact aliasing the repair
    // targets. Assert it loudly rather than silently running a non-alias
    // environment.
    let construction_raw = copy.to_string_lossy().to_string();
    let canonical = copy.canonicalize().expect("copy exists");
    assert_ne!(
        construction_raw,
        canonical.to_string_lossy(),
        "premise broken: the aliased TMPDIR construction must differ from its canonical form"
    );

    let output = std::process::Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&copy)
        .arg(&head)
        .current_dir(&claim)
        .output()
        .expect("git worktree add spawn");
    assert!(
        output.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let registered = registered_worktree_paths(&claim);
    assert!(
        registered.iter().any(|path| {
            Path::new(path).canonicalize().ok().as_deref() == Some(canonical.as_path())
        }),
        "premise: git must register the copy under its canonical spelling: {registered:?}"
    );

    // Registration-only residue: dir gone, registration persists.
    std::fs::remove_dir_all(&copy).expect("remove copy dir");
    let probe = ProcessCheckRunner.copy_is_registered(&claim, &copy);
    assert_eq!(
        probe,
        RegistrationProbe::Registered,
        "gone-dir registration must be detected across the aliased spelling"
    );
}

/// #1454 O1 (oracle major): an unanswerable git must NOT silently conclude
/// "no registration". The guard treats the probe as UNKNOWN: loud warn
/// naming the sweeper, best-effort registration removal, and the
/// dir-existence cleanup still runs (fail-loud, never silent). Real git is
/// driven into the nonzero path via a claim dir that is not a repository
/// (premise asserted below); the warn is captured with a tracing subscriber.
#[test]
fn guard_unanswerable_git_registration_is_loud_and_falls_back_to_dir_cleanup() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let copy = tmp.path().join("verify-worktrees").join("flow-fmt-1");
    std::fs::create_dir_all(&copy).expect("copy dir");
    // A claim dir that EXISTS but is not a git repository → `git worktree
    // list` exits nonzero (unanswerable), unlike a nonexistent cwd which
    // would make git fail to SPAWN at all.
    let claim = tmp.path().join("claim-not-a-repo");
    std::fs::create_dir_all(&claim).expect("claim dir");

    // Premise: real git is unanswerable here (nonzero exit).
    let probe = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&claim)
        .output()
        .expect("git probe spawn");
    assert!(
        !probe.status.success(),
        "premise broken: git answered in a non-repository claim dir"
    );

    let buf = TestBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let _tracing_guard = tracing::subscriber::set_default(subscriber);
    {
        let _copy_guard = DetachedCopyGuard {
            runner: &ProcessCheckRunner,
            claim: claim.clone(),
            copy: copy.clone(),
        };
    }
    let log = buf.string();
    assert!(
        log.contains("could not determine"),
        "the unknown-registration path must warn loudly: {log}"
    );
    assert!(
        log.contains("sweeper"),
        "the warn must name the worktree sweeper for reclaim: {log}"
    );
    assert!(
        !copy.exists(),
        "dir-existence cleanup must still run on the unknown path"
    );
}

/// #1454 O3 (oracle major): a DANGLING `.git` SYMLINK must trip the walk-up
/// fallback. `Path::exists()` follows the symlink → dangling → false →
/// allowed (the old hole); `symlink_metadata` (no follow) counts the symlink
/// as PRESENT → refuse. Premise asserted: real git exits nonzero here (a
/// dangling .git symlink is not a repository), so the fallback is the only
/// answer source.
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_receipts_root_with_dangling_git_symlink_refuses_via_walk_up_fallback() {
    let (_root, _guard) = with_run_root();
    let home = tempfile::tempdir().expect("home tempdir");
    std::os::unix::fs::symlink(home.path().join("missing-gitdir"), home.path().join(".git"))
        .expect("dangling .git symlink");

    let probe = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(home.path())
        .output()
        .expect("git probe spawn");
    assert!(
        !probe.status.success(),
        "premise broken: git succeeded with a dangling .git symlink ({})",
        String::from_utf8_lossy(&probe.stderr).trim()
    );

    let server = MemoryServer::new_with_home_for_test(
        home.path().join("global.db"),
        None,
        home.path().to_path_buf(),
    )
    .expect("test server at a home with a dangling .git symlink");
    let worktree = tempfile::tempdir().expect("worktree tempdir");
    let flow_id = "flow_run-dangling-git-symlink";
    seed_claim(
        &server,
        flow_id,
        worktree.path().to_str().expect("utf8 worktree"),
    );

    let runner = fake_runner();
    let err = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("a dangling .git symlink must not fail open: the walk-up fallback refuses");
    assert!(err.contains("resolves inside a git worktree"), "{err}");
    assert!(err.contains("authorable location"), "{err}");
}

/// In-memory tracing writer for the fail-loud warn capture (same pattern as
/// server_state/accessors.rs + bootstrap/clean_cli.rs).
#[derive(Clone, Default)]
struct TestBuf {
    inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl TestBuf {
    fn string(&self) -> String {
        String::from_utf8_lossy(&self.inner.lock().unwrap()).to_string()
    }
}

impl std::io::Write for TestBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TestBuf {
    type Writer = TestBuf;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// #1454 P1 (oracle major, round 5): the porcelain registration parser must
/// not miss a worktree whose path contains a newline. `git worktree list
/// --porcelain` emits the path as a NEWLINE-TERMINATED line, so a `\n` in a
/// TACHI_HOME-derived copy path splits the record and the `.lines()` parser
/// reports NotRegistered — the oracle's silent registration leak (reproduced
/// against real git: `git worktree add` accepts a newline-bearing path).
/// The `--porcelain -z` form NUL-terminates each field, so the path survives
/// intact inside its `worktree <path>` field.
///
/// Layer honesty: THIS test carries the PARSER layer (a registration that
/// already exists must be seen); the copy-CREATION layer (the server must
/// refuse to ever mint such a path) is carried by
/// `run_refuses_copy_creation_when_home_path_contains_newline`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn porcelain_parser_sees_registration_with_newline_in_path() {
    let (_root, _guard) = with_run_root();
    let (_main, claim, head) = linked_claim_fixture(true);
    let copy = _main.path().join("copy\nwith-newline");
    let add = std::process::Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&copy)
        .arg(&head)
        .current_dir(&claim)
        .output()
        .expect("git worktree add");
    assert!(
        add.status.success(),
        "premise broken: git refused a newline-bearing worktree path: {}",
        String::from_utf8_lossy(&add.stderr).trim()
    );

    // Premise: the OLD `.lines()` parser misses this registration (the
    // oracle's blind spot) — the newline splits the porcelain record.
    let old = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&claim)
        .output()
        .expect("git worktree list");
    let old_matched = String::from_utf8_lossy(&old.stdout).lines().any(|line| {
        line.strip_prefix("worktree ")
            .is_some_and(|p| p == copy.to_str().unwrap())
    });
    assert!(
        !old_matched,
        "premise broken: the newline-splitting parser unexpectedly matched"
    );

    let probe = ProcessCheckRunner.copy_is_registered(&claim, &copy);
    assert_eq!(
        probe,
        RegistrationProbe::Registered,
        "the -z porcelain parser must see the newline-bearing registration"
    );
}

/// #1454 P1 defense in depth (oracle major, round 5): a TACHI_HOME (or
/// claim) whose path contains a newline must be REFUSED LOUDLY at copy
/// creation — such a path is never legitimate here, and a registration minted
/// under it is the silent leak the parser fix defends against. Layer honesty:
/// the leak-prevention LAYER carried by this test is the copy-creation
/// refusal (the parser layer is proven by
/// `porcelain_parser_sees_registration_with_newline_in_path`); after the
/// refusal there is nothing registered to leak.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_refuses_copy_creation_when_home_path_contains_newline() {
    let (_root, _guard) = with_run_root();
    let base = tempfile::tempdir().expect("home base tempdir");
    let home_path = base.path().join("home\nwith-newline");
    std::fs::create_dir_all(&home_path).expect("create newline home");
    let server =
        MemoryServer::new_with_home_for_test(home_path.join("global.db"), None, home_path.clone())
            .expect("test server at a newline-bearing home");
    let (_main, claim, _head) = linked_claim_fixture(true);
    let flow_id = "flow_newline-home-refusal";
    seed_claim(&server, flow_id, claim.to_str().expect("utf8 claim"));

    let runner = RealCopyKillIgnoringRunner {
        claim: claim.clone(),
    };
    let err = run_with_runner(
        &server,
        &runner,
        flow_id,
        "fmt",
        check_kind_argv("fmt").expect("fmt argv"),
        Duration::from_secs(60),
    )
    .await
    .expect_err("a newline-bearing copy path must be refused loudly, never created");

    assert!(
        err.contains("newline"),
        "typed refusal must name the character: {err}"
    );
    assert!(
        err.contains("copy"),
        "typed refusal must name the copy path: {err}"
    );

    // Nothing to leak: the refusal fired BEFORE `git worktree add`, so no
    // registration exists in the claim repo and no copy dir was created.
    let home = server.tachi_home_dir();
    let leftovers: Vec<_> = std::fs::read_dir(home.join("verify-worktrees"))
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "no copy dir may exist after the refusal: {leftovers:?}"
    );
    let registered = registered_worktree_paths(&claim);
    assert!(
        !registered
            .iter()
            .any(|path| path.contains("verify-worktrees")),
        "no copy registration may exist after the refusal: {registered:?}"
    );
    assert_eq!(
        runner.copy_is_registered(&claim, &home.join("verify-worktrees").join("fmt")),
        RegistrationProbe::NotRegistered,
        "registration probe after the refusal must be NotRegistered"
    );
}

/// #1454 P3 (oracle major, round 5): the G1 walk-up `.git` fallback must
/// fail CLOSED when an ancestor cannot be inspected. Pre-repair every
/// `symlink_metadata` error — including PermissionDenied on an unreadable
/// ancestor that CONTAINS a `.git` entry — was treated as "no .git here" and
/// the walk walked past it, letting the receipts root resolve inside a
/// repo-shaped authorable location. Post-repair: `NotFound` keeps walking;
/// any other error refuses with a typed error naming the unreadable
/// ancestor.
///
/// Unix-only: Windows permission semantics do not produce `PermissionDenied`
/// from a 000-mode directory the same way — Windows behavior is UNTESTED per
/// the platform-limited rule.
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn receipts_root_under_unreadable_ancestor_with_git_refuses_closed() {
    use std::os::unix::fs::PermissionsExt;

    let base = tempfile::tempdir().expect("base tempdir");
    // The unreadable ancestor CONTAINS a `.git` entry — the repo-shaped
    // location the fail-open walk walked past.
    let secret = base.path().join("secret");
    std::fs::create_dir_all(&secret).expect("create secret dir");
    std::fs::write(secret.join(".git"), "gitdir: /nonexistent\n").expect("write .git");
    // Home sits UNDER the unreadable ancestor, so the receipts root's first
    // existing ancestor is `secret`.
    let home = secret.join("home");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000))
        .expect("chmod 000 secret");

    let err = ensure_receipts_root_not_in_repo(&home)
        .expect_err("an unreadable ancestor containing .git must refuse closed, never fail open");
    assert!(
        err.contains("cannot inspect") || err.contains("inside a git worktree"),
        "typed refusal must name the unreadable ancestor (or the containment): {err}"
    );
}
