use super::*;
use crate::managed_run_control::test_hooks::{install_status_io_hook, StatusIoHookStage};
use crate::managed_run_control::AnchoredRunStatus;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "complete_ops::handler::status_fence_tests::status_fence_child";
const RUN_ID: &str = "fixture-status-run";
const LOCK: &str = ".status-mutation.lock";

fn generic(run: &Path, anchored: Option<&AnchoredRunStatus>) {
    let extra = Some(json!({"state":"TASK_STATE_WORKING", "result":"generic-committed"}));
    if let Some(anchor) = anchored {
        crate::dispatch_ops::write_status_json_with_managed_anchor(
            run, RUN_ID, true, None, None, "approved", None, None, None, None, extra, anchor,
        );
    } else {
        crate::dispatch_ops::write_status_json(
            run, RUN_ID, true, None, None, "approved", None, None, None, None, extra,
        );
    }
}

fn seed(root: &Path) -> PathBuf {
    let run = root.canonicalize().unwrap().join("run");
    fs::create_dir(&run).unwrap();
    fs::write(
        run.join("status.json"),
        serde_json::to_vec(&json!({
            "dispatch_id": RUN_ID, "state":"TASK_STATE_WORKING", "status_revision":0
        }))
        .unwrap(),
    )
    .unwrap();
    crate::dispatch_ops::status_fence_acp_fixture(&run, true).unwrap();
    run
}

fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct OwnedChild(Child);
impl OwnedChild {
    fn wait_success(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "status fixture child exited {status}");
                return;
            }
            assert!(Instant::now() < deadline, "status fixture child timeout");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn spawn(root: &Path, run: &Path, mode: &str, pause: bool) -> OwnedChild {
    let root = root.canonicalize().unwrap();
    let home = root.join(format!("home-{mode}"));
    fs::create_dir_all(&home).unwrap();
    let output = File::create(root.join(format!("{mode}.log"))).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .current_dir(&root)
        .args(["--exact", CHILD, "--nocapture"])
        .env("HOME", &home)
        .env("TACHI_HOME", &home)
        .env("FENCE_FIXTURE_ROOT", &root)
        .env("FENCE_FIXTURE_RUN", run)
        .env("FENCE_FIXTURE_MODE", mode)
        .env("FENCE_FIXTURE_PAUSE", if pause { "yes" } else { "no" })
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output));
    OwnedChild(command.spawn().unwrap())
}

#[test]
fn status_fence_child() {
    let Some(root) = std::env::var_os("FENCE_FIXTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let run = PathBuf::from(std::env::var_os("FENCE_FIXTURE_RUN").unwrap());
    assert!(run.starts_with(&root));
    let mode = std::env::var("FENCE_FIXTURE_MODE").unwrap();
    if std::env::var("FENCE_FIXTURE_PAUSE").unwrap() == "yes" {
        let ready = root.join("ready");
        let release = root.join("release");
        install_status_io_hook(StatusIoHookStage::AfterStatusRead, run.clone(), move |_| {
            fs::write(ready, b"locked-read").unwrap();
            wait_file(&release);
        });
    }
    if mode != "generic" {
        let contended = root.join(format!("{mode}.contended"));
        install_status_io_hook(StatusIoHookStage::LockWouldBlock, run.clone(), move |_| {
            fs::write(contended, b"actual-try-lock-would-block").unwrap();
        });
    }
    fs::write(root.join(format!("{mode}.started")), b"started").unwrap();
    match mode.as_str() {
        "generic" => generic(&run, None),
        "completion" => persist_resolved_completion_receipt_at(
            &run,
            RUN_ID,
            "TASK_STATE_DONE",
            "fixture-eval",
            true,
        )
        .unwrap(),
        "reconciliation" => {
            assert!(crate::managed_run_epoch::status_fence_reconciliation_fixture(&run).unwrap());
        }
        "acp" => crate::dispatch_ops::status_fence_acp_fixture(&run, false).unwrap(),
        "managed" => crate::managed_run_control::mark_managed_custom_start(
            &run,
            RUN_ID,
            &crate::managed_run_epoch::ManagedRunIdentityInput {
                controller_epoch_id: "fixture-epoch".into(),
                assignment_ref: "fixture-assignment".into(),
                assignment_identity_digest: None,
                execution_grant_ref: "fixture-grant".into(),
                exec_env_ref: None,
                launch_spec_digest: None,
                backend_name: "fixture".into(),
                backend_metadata_digest: None,
            },
        )
        .unwrap(),
        "route" => crate::dispatch_ops::stamp_route_decision_id(&run, "fixture-route").unwrap(),
        "anchored" => {
            let anchor = AnchoredRunStatus::open(&run).unwrap();
            generic(&run, Some(&anchor));
        }
        "timeout" => {
            let before = fs::read(run.join("status.json")).unwrap();
            let start = Instant::now();
            assert!(
                crate::dispatch_ops::stamp_route_decision_id(&run, "must-not-commit")
                    .unwrap_err()
                    .contains("timed out")
            );
            assert!(start.elapsed() >= Duration::from_secs(5));
            assert_eq!(fs::read(run.join("status.json")).unwrap(), before);
        }
        _ => panic!("unknown private fixture mode"),
    }
    fs::write(root.join(format!("{mode}.done")), b"done").unwrap();
}

#[test]
fn status_fence_serializes_actual_writer_processes_and_releases_after_owner_death() {
    for mode in [
        "completion",
        "route",
        "anchored",
        "managed",
        "acp",
        "reconciliation",
    ] {
        for kill_owner in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let run = seed(tmp.path());
            let mut a = spawn(tmp.path(), &run, "generic", true);
            wait_file(&tmp.path().join("ready"));
            let lock_inode = fs::metadata(run.join(LOCK)).unwrap().ino();
            let mut b = spawn(tmp.path(), &run, mode, false);
            let deadline = Instant::now() + Duration::from_secs(12);
            while !tmp.path().join(format!("{mode}.contended")).exists() {
                assert!(
                    b.0.try_wait().unwrap().is_none(),
                    "second writer completed without actual lock contention"
                );
                assert!(
                    Instant::now() < deadline,
                    "second writer never reached actual lock contention"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                b.0.try_wait().unwrap().is_none(),
                "second writer passed held fence"
            );
            assert!(!tmp.path().join(format!("{mode}.done")).exists());
            if kill_owner {
                a.0.kill().unwrap();
                assert!(!a.0.wait().unwrap().success());
            } else {
                fs::write(tmp.path().join("release"), b"release").unwrap();
                a.wait_success();
            }
            b.wait_success();
            let status: Value =
                serde_json::from_slice(&fs::read(run.join("status.json")).unwrap()).unwrap();
            assert_eq!(status["status_revision"], if kill_owner { 1 } else { 2 });
            if !kill_owner || mode == "anchored" {
                assert_eq!(status["result"], "generic-committed");
            } else {
                assert!(
                    status.get("result").is_none(),
                    "killed writer published its uncommitted result"
                );
            }
            if mode == "reconciliation" {
                let transitions = status[crate::managed_run_epoch::RECONCILIATION_KEY]
                    ["transitions"]
                    .as_array()
                    .unwrap();
                assert_eq!(transitions.len(), 1);
                assert_eq!(transitions[0]["verdict"], "inconsistent");
                assert_eq!(
                    transitions[0]["reconciling_controller_epoch_id"],
                    "fixture-reconciling-epoch"
                );
                assert_eq!(transitions[0]["prior_state"], "TASK_STATE_WORKING");
                assert_eq!(transitions[0]["execution_state"], "unknown");
                assert_eq!(transitions[0]["control_state"], "unavailable");
            }
            if mode == "managed" {
                assert_eq!(status["execution_classification"], "managed_custom");
            }
            if mode == "acp" {
                assert_eq!(
                    status["identity_receipt"]["observed"]["effective"]["model"],
                    "gpt-5.5"
                );
            }
            if mode == "completion" {
                assert_eq!(
                    status["resolved_completion"]["eval_ledger_id"],
                    "fixture-eval"
                );
            }
            if mode == "route" {
                assert_eq!(status["route_decision_id"], "fixture-route");
            }
            assert_eq!(fs::metadata(run.join(LOCK)).unwrap().ino(), lock_inode);
            eprintln!(
                "STATUS_FENCE mode={mode} killed={kill_owner} revision={}",
                status["status_revision"]
            );
        }
    }
}

#[test]
fn status_fence_contention_timeout_never_writes_unlocked() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seed(tmp.path());
    let mut a = spawn(tmp.path(), &run, "generic", true);
    wait_file(&tmp.path().join("ready"));
    let mut b = spawn(tmp.path(), &run, "timeout", false);
    b.wait_success();
    fs::write(tmp.path().join("release"), b"release").unwrap();
    a.wait_success();
}

#[test]
fn status_fence_invalid_lock_and_failed_read_preserve_committed_bytes() {
    for shape in [
        "symlink",
        "directory",
        "hardlink",
        "permissions",
        "malformed",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let run = seed(tmp.path());
        let leaf = run.join(LOCK);
        match shape {
            "symlink" => {
                fs::write(tmp.path().join("other"), b"private").unwrap();
                symlink(tmp.path().join("other"), &leaf).unwrap();
            }
            "directory" => fs::create_dir(&leaf).unwrap(),
            "hardlink" => {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&leaf)
                    .unwrap();
                fs::hard_link(&leaf, tmp.path().join("other")).unwrap();
            }
            "permissions" => {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o644)
                    .open(&leaf)
                    .unwrap();
            }
            "malformed" => fs::write(run.join("status.json"), b"{invalid").unwrap(),
            _ => unreachable!(),
        }
        let before = fs::read(run.join("status.json")).unwrap();
        assert!(
            crate::dispatch_ops::stamp_route_decision_id(&run, "refused").is_err(),
            "{shape}"
        );
        generic(&run, None);
        assert_eq!(
            fs::read(run.join("status.json")).unwrap(),
            before,
            "{shape}"
        );
        assert!(persist_resolved_completion_receipt_at(
            &run,
            RUN_ID,
            "TASK_STATE_DONE",
            "refused",
            true
        )
        .is_err());
        assert_eq!(fs::read(run.join("status.json")).unwrap(), before);
    }
    let tmp = tempfile::tempdir().unwrap();
    let absent = tmp.path().join("absent");
    generic(&absent, None);
    assert!(!absent.exists());
}

#[test]
fn status_fence_rejects_lock_replacement_before_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seed(tmp.path());
    let before = fs::read(run.join("status.json")).unwrap();
    install_status_io_hook(StatusIoHookStage::BeforeAtomicRename, run.clone(), |run| {
        fs::rename(run.join(LOCK), run.join("old-lock")).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(run.join(LOCK))
            .unwrap();
    });
    assert!(
        crate::dispatch_ops::stamp_route_decision_id(&run, "refused")
            .unwrap_err()
            .contains("changed")
    );
    assert_eq!(fs::read(run.join("status.json")).unwrap(), before);
    assert!(!fs::read_dir(&run).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("status.json.tmp.")));
}

#[test]
fn status_fence_retains_ordinary_alias_and_accepted_managed_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seed(tmp.path());
    let alias = tmp.path().join("alias");
    symlink(&run, &alias).unwrap();
    assert!(AnchoredRunStatus::open(&alias).is_err());
    crate::dispatch_ops::stamp_route_decision_id(&alias, "alias-accepted").unwrap();
    let replacement = tmp.path().join("replacement");
    fs::create_dir(&replacement).unwrap();
    fs::write(replacement.join("status.json"), b"replacement").unwrap();
    let alias_for_hook = alias.clone();
    let replacement_for_hook = replacement.clone();
    install_status_io_hook(
        StatusIoHookStage::AfterDirectoryValidation,
        run.clone(),
        move |_| {
            fs::remove_file(&alias_for_hook).unwrap();
            symlink(replacement_for_hook, alias_for_hook).unwrap();
        },
    );
    crate::dispatch_ops::stamp_route_decision_id(&alias, "anchored-original").unwrap();
    assert_eq!(
        fs::read(replacement.join("status.json")).unwrap(),
        b"replacement"
    );
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let displaced = tmp.path().join("displaced");
    let displaced_for_hook = displaced.clone();
    let replacement_for_hook = replacement.clone();
    install_status_io_hook(
        StatusIoHookStage::BeforeAtomicRename,
        run.clone(),
        move |run| {
            fs::rename(run, displaced_for_hook).unwrap();
            fs::rename(replacement_for_hook, run).unwrap();
        },
    );
    generic(&run, Some(&anchor));
    assert_eq!(fs::read(run.join("status.json")).unwrap(), b"replacement");
    let status: Value =
        serde_json::from_slice(&fs::read(displaced.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["result"], "generic-committed");
    assert_eq!(status["route_decision_id"], "anchored-original");
}

#[test]
fn status_fence_panic_after_read_releases_lock_without_committing() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seed(tmp.path());
    let before = fs::read(run.join("status.json")).unwrap();
    install_status_io_hook(StatusIoHookStage::AfterStatusRead, run.clone(), |_| {
        panic!("private read fault")
    });
    let result = std::panic::catch_unwind(|| {
        crate::dispatch_ops::stamp_route_decision_id(&run, "not-committed")
    });
    assert!(result.is_err());
    assert_eq!(fs::read(run.join("status.json")).unwrap(), before);
    crate::dispatch_ops::stamp_route_decision_id(&run, "after-panic").unwrap();
    let status: Value =
        serde_json::from_slice(&fs::read(run.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["status_revision"], 1);
    assert_eq!(status["route_decision_id"], "after-panic");
}
