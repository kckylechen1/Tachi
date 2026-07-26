//! Discriminating tests for the `detect-and-reject` postflight gate (#894 S2e).
//!
//! The fixture is deliberately **unsandboxed**: it simulates exactly the
//! scenario this gate exists for — a same-UID worker Tachi cannot stop from
//! writing. Each mutation below is applied directly to the lease workspace
//! between the pre-image capture and the gate run, i.e. every test plays the
//! part of a worker that ignored its contract.
//!
//! Eight mutation surfaces must each be caught, loudly, with no patch emitted:
//! tracked edit · untracked create · **ignored-file write** · **git metadata**
//! (in-tree and a linked worktree's external `gitdir`) · symlink retarget ·
//! deletion · **xattr** · **mutate-then-restore** (content *and* mtime put
//! back). The last three are precisely the classes a `git diff` cannot see —
//! if this gate is ever reduced to a `git diff`, those tests go red.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::liveness::DescendantLiveness;
use super::manifest::{CtimeWitness, DeltaKind, FsTime};
use super::{
    apply_verdict, manifest, naming, rejection_log_message, BlockReason, ClockBarrierEvidence,
    FileQuarantineSink, GateOutcome, GateVerdict, PostflightGate, QuarantineSink, RejectReason,
    WriteContract,
};

// ─── fakes ──────────────────────────────────────────────────────────────────

/// The worker tree is gone (what the dispatch runner guarantees by waiting on
/// the child before the gate runs).
struct Reaped;
impl DescendantLiveness for Reaped {
    fn any_alive(&self) -> Result<bool, String> {
        Ok(false)
    }
    fn describe(&self) -> String {
        "fake probe: worker tree reaped".to_string()
    }
}

/// A descendant is still running.
struct StillRunning;
impl DescendantLiveness for StillRunning {
    fn any_alive(&self) -> Result<bool, String> {
        Ok(true)
    }
    fn describe(&self) -> String {
        "fake probe: a descendant is still running".to_string()
    }
}

/// The probe itself failed — must fail closed, never pass.
struct ProbeBroken;
impl DescendantLiveness for ProbeBroken {
    fn any_alive(&self) -> Result<bool, String> {
        Err("probe exploded".to_string())
    }
    fn describe(&self) -> String {
        "fake probe: broken".to_string()
    }
}

// ─── fixture ────────────────────────────────────────────────────────────────

/// A lease workspace plus a PARENT-side dir for the pre-image (outside anything
/// the "worker" can write).
struct Fixture {
    workspace: TempDir,
    parent: TempDir,
}

impl Fixture {
    /// A workspace with an in-tree `.git` directory (the plain-repo shape).
    fn new() -> Fixture {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let parent = tempfile::tempdir().expect("parent tempdir");
        let root = workspace.path();

        fs::create_dir_all(root.join("src")).expect("src");
        fs::write(root.join("src/tracked.rs"), b"fn main() {}\n").expect("tracked");
        fs::write(root.join(".gitignore"), b"ignored/\n").expect("gitignore");
        fs::create_dir_all(root.join("ignored")).expect("ignored dir");
        fs::write(root.join("ignored/build.log"), b"cached build output\n").expect("ignored file");
        fs::create_dir_all(root.join(".git/refs/heads")).expect("git dir");
        fs::write(root.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
        fs::write(root.join(".git/index"), b"binary-index-v1").expect("index");
        #[cfg(unix)]
        std::os::unix::fs::symlink("src/tracked.rs", root.join("link.rs")).expect("symlink");

        Fixture { workspace, parent }
    }

    /// The linked-worktree shape: `.git` is a FILE pointing at an external
    /// metadata dir, which is where a worktree's index/HEAD actually live.
    fn linked_worktree() -> Fixture {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let parent = tempfile::tempdir().expect("parent tempdir");
        let root = workspace.path();
        let gitdir = parent.path().join("worktrees/lane-a");

        fs::create_dir_all(&gitdir).expect("external gitdir");
        fs::write(gitdir.join("HEAD"), b"ref: refs/heads/lane-a\n").expect("HEAD");
        fs::write(gitdir.join("index"), b"binary-index-v1").expect("index");
        fs::write(
            root.join(".git"),
            format!("gitdir: {}\n", gitdir.display()).as_bytes(),
        )
        .expect(".git file");
        fs::write(root.join("tracked.txt"), b"hello\n").expect("tracked");

        Fixture { workspace, parent }
    }

    fn ws(&self) -> &Path {
        self.workspace.path()
    }

    fn parent(&self) -> &Path {
        self.parent.path()
    }

    fn gitdir(&self) -> PathBuf {
        self.parent.path().join("worktrees/lane-a")
    }

    fn preimage_path(&self) -> PathBuf {
        self.parent.path().join("preimages/env-test.json")
    }

    fn quarantine_dir(&self) -> PathBuf {
        self.parent.path().join("quarantine")
    }

    fn gate(&self, contract: WriteContract) -> PostflightGate {
        PostflightGate::new("env-test", self.ws(), self.preimage_path(), contract)
    }

    /// Capture the pre-image, let the "worker" run `mutate`, then gate it.
    fn run_worker(&self, contract: WriteContract, mutate: impl FnOnce(&Path)) -> GateOutcome {
        let gate = self.gate(contract);
        gate.capture_preimage().expect("pre-image capture");
        mutate(self.ws());
        gate.run(&Reaped).expect("gate run")
    }
}

fn assert_caught(outcome: &GateOutcome, path_suffix: &str, kind: DeltaKind) {
    let GateVerdict::Rejected { reason, deltas, .. } = &outcome.verdict else {
        panic!(
            "an unsandboxed worker mutated the workspace and the gate did NOT reject it: {:?}",
            outcome.verdict
        );
    };
    assert_eq!(*reason, RejectReason::ProhibitedDelta);
    assert!(
        deltas
            .iter()
            .any(|d| d.path.ends_with(path_suffix) && d.kind == kind),
        "expected a {} delta at a path ending in {path_suffix}; got {:#?}",
        kind.as_str(),
        deltas
    );
    // The load-bearing half: a rejected run yields NO patch and NO result.
    assert!(!outcome.artifacts_released());
    assert!(outcome
        .release("the patch this worker produced")
        .is_err_and(|e| e.contains("REJECTED")));
    assert!(outcome.lease_quarantine_required());
    // And it is loud.
    let message = outcome.failure_message().expect("loud failure message");
    assert!(message.contains("REJECTED"), "not loud enough: {message}");
}

// ─── the clean run ──────────────────────────────────────────────────────────

#[test]
fn clean_run_passes_and_releases_the_patch() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |_ws| {
        // A well-behaved worker: reads only.
        let _ = fs::read(_ws.join("src/tracked.rs")).expect("read is allowed");
    });

    let GateVerdict::Clean { entries_checked } = &outcome.verdict else {
        panic!(
            "a read-only worker must pass the gate: {:?}",
            outcome.verdict
        );
    };
    assert!(*entries_checked > 5, "the whole tree is fingerprinted");
    assert!(outcome.artifacts_released());
    assert_eq!(
        outcome.release("patch").expect("clean run releases"),
        "patch"
    );
    assert!(!outcome.lease_quarantine_required());
    assert!(outcome.failure_message().is_none());
}

// ─── the eight mutation surfaces ────────────────────────────────────────────

#[test]
fn surface_1_tracked_file_edit_is_caught() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        fs::write(ws.join("src/tracked.rs"), b"fn main() { evil() }\n").expect("edit");
    });
    assert_caught(&outcome, "src/tracked.rs", DeltaKind::ContentChanged);
}

#[test]
fn surface_2_untracked_file_creation_is_caught() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        fs::write(ws.join("src/dropped.rs"), b"// left behind\n").expect("create");
    });
    assert_caught(&outcome, "src/dropped.rs", DeltaKind::Added);
}

#[test]
fn surface_3_ignored_file_write_is_caught() {
    // `ignored/` is in .gitignore — `git diff` and `git status` both report a
    // CLEAN tree after this write. The gate must still catch it.
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        fs::write(ws.join("ignored/build.log"), b"tampered\n").expect("write ignored");
    });
    assert_caught(&outcome, "ignored/build.log", DeltaKind::ContentChanged);
}

#[test]
fn surface_4a_in_tree_git_metadata_write_is_caught() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        fs::write(ws.join(".git/index"), b"binary-index-v2-rewritten").expect("write index");
    });
    assert_caught(&outcome, ".git/index", DeltaKind::ContentChanged);
}

#[test]
fn surface_4b_external_gitdir_write_is_caught() {
    // In a linked git worktree the index/HEAD live OUTSIDE the workspace. A
    // walk of the workspace alone would be blind to a ref/index rewrite; the
    // manifest follows the `gitdir:` pointer precisely so it is not.
    let fx = Fixture::linked_worktree();
    let gitdir = fx.gitdir();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |_ws| {
        fs::write(gitdir.join("HEAD"), b"ref: refs/heads/attacker\n").expect("rewrite HEAD");
    });
    assert_caught(&outcome, "gitdir/HEAD", DeltaKind::ContentChanged);
}

#[test]
fn surface_4c_a_worker_cannot_repoint_the_gitdir_walk_root() {
    // The `.git` FILE that names the second walk root lives INSIDE the workspace,
    // i.e. the worker can rewrite it. If the postflight re-read it, the worker
    // would choose what the parent walks: point it at a decoy and the real gitdir
    // is never re-scanned (the rewrite hides itself); point it at `/` and the
    // parent hashes the whole disk (DoS). So the root is resolved once, before the
    // spawn, and pinned in the pre-image.
    let fx = Fixture::linked_worktree();
    let gitdir = fx.gitdir();
    let decoy = fx.parent().join("decoy-gitdir");
    fs::create_dir_all(&decoy).expect("decoy dir");
    fs::write(decoy.join("decoy-HEAD"), b"ref: refs/heads/decoy\n").expect("decoy HEAD");

    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        // The worker rewrites BOTH: the real gitdir's HEAD, and the pointer that
        // is supposed to lead the parent to it.
        fs::write(gitdir.join("HEAD"), b"ref: refs/heads/attacker\n").expect("rewrite HEAD");
        fs::write(
            ws.join(".git"),
            format!("gitdir: {}\n", decoy.display()).as_bytes(),
        )
        .expect("repoint .git");
    });

    // The pinned root was walked, so the real HEAD rewrite is still convicted...
    assert_caught(&outcome, "gitdir/HEAD", DeltaKind::ContentChanged);
    // ...the redirect itself is convicted as an ordinary content delta on the
    // `.git` file...
    assert!(
        outcome
            .deltas()
            .iter()
            .any(|d| d.path == "workspace/.git" && d.kind == DeltaKind::ContentChanged),
        "rewriting `.git` is itself a prohibited delta: {:#?}",
        outcome.deltas()
    );
    // ...and the parent never followed the worker's pointer.
    assert!(
        !outcome.deltas().iter().any(|d| d.path.contains("decoy")),
        "the postflight walked a root the WORKER chose: {:#?}",
        outcome.deltas()
    );
}

#[cfg(unix)]
#[test]
fn surface_5_symlink_retarget_is_caught() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        fs::remove_file(ws.join("link.rs")).expect("unlink");
        std::os::unix::fs::symlink("/etc/passwd", ws.join("link.rs")).expect("retarget");
    });
    assert_caught(&outcome, "link.rs", DeltaKind::SymlinkTargetChanged);
}

#[test]
fn surface_6_deletion_is_caught() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        fs::remove_file(ws.join("src/tracked.rs")).expect("delete");
    });
    assert_caught(&outcome, "src/tracked.rs", DeltaKind::Removed);
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[test]
fn surface_7_xattr_change_is_caught_by_the_xattr_fingerprint_itself() {
    // Content, size, mode and mtime are all untouched here — only an extended
    // attribute changed. `git diff` sees nothing at all.
    //
    // This test is DISCRIMINATING and must stay that way: `setxattr` also bumps
    // the inode's ctime, so accepting `XattrChanged || MetadataChanged` would go
    // green even if the xattr fingerprint were deleted from the manifest — the
    // one surface of the eight whose test would not hold its own mechanism to
    // account. So: only `XattrChanged` passes, and the `xattrs …` facet must be
    // present. This cfg gate matches the cfg gate on `manifest::xattr_digest`'s
    // real implementation exactly, so wherever the mechanism is claimed, it is
    // also proven.
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        set_xattr(
            &ws.join("src/tracked.rs"),
            "user.tachi.test",
            b"planted-by-the-worker",
        );
    });
    let GateVerdict::Rejected { deltas, .. } = &outcome.verdict else {
        panic!("an xattr write must reject: {:?}", outcome.verdict);
    };
    let delta = deltas
        .iter()
        .find(|d| d.path.ends_with("src/tracked.rs"))
        .unwrap_or_else(|| panic!("no delta for the xattr'd file: {deltas:#?}"));
    assert_eq!(
        delta.kind,
        DeltaKind::XattrChanged,
        "the xattr fingerprint — not the incidental ctime bump — must be what classifies this: {}",
        delta.detail
    );
    assert!(
        delta.facets.iter().any(|f| f.starts_with("xattrs ")),
        "the receipt must show the xattr digest changing, or the manifest is not actually \
         reading xattrs: {:?}",
        delta.facets
    );
    // The content bytes really are untouched — so content is not what caught it.
    assert!(
        !delta.facets.iter().any(|f| f.starts_with("content ")),
        "the fixture changed the file's bytes, so this test would pass for the wrong reason: {:?}",
        delta.facets
    );
    assert!(!outcome.artifacts_released());
    assert!(outcome.lease_quarantine_required());
}

#[cfg(unix)]
#[test]
fn surface_8_mutate_then_restore_is_caught_by_ctime_alone() {
    // The hardest case, and the reason `git diff` is not the mechanism: the
    // worker edits a file, puts the ORIGINAL BYTES BACK, and restores atime and
    // mtime to the nanosecond with utimensat(). Content hash, size, mode, inode
    // AND mtime all match the pre-image afterwards — every field a well-behaved
    // tool would look at says "unchanged". Only ctime still testifies, because
    // POSIX has no call that sets it and the one that restores mtime
    // (utimensat) bumps it.
    let fx = Fixture::new();
    let target = fx.ws().join("src/tracked.rs");
    let original = fs::read(&target).expect("original bytes");
    let (atime, mtime) = access_and_modify_times(&target);

    let outcome = fx.run_worker(WriteContract::DetectAndReject, |_ws| {
        fs::write(&target, b"// transient tamper\n").expect("mutate");
        fs::write(&target, &original).expect("restore bytes");
        set_times(&target, atime, mtime);
    });

    let GateVerdict::Rejected { deltas, .. } = &outcome.verdict else {
        panic!(
            "mutate-then-restore must NOT pass as clean: {:?}",
            outcome.verdict
        );
    };
    let delta = deltas
        .iter()
        .find(|d| d.path.ends_with("src/tracked.rs"))
        .unwrap_or_else(|| panic!("the restore hid the mutation from the gate: {deltas:#?}"));
    assert_eq!(delta.kind, DeltaKind::MetadataChanged);
    // The restore really did put mtime back — so mtime is NOT what caught this.
    assert!(
        !delta.facets.iter().any(|f| f.starts_with("mtime ")),
        "the fixture failed to restore mtime exactly, so this test would pass for the wrong \
         reason: {:?}",
        delta.facets
    );
    // ctime is the field doing the work. Drop it from the fingerprint and this
    // whole mutation class becomes invisible.
    assert!(
        delta.facets.iter().any(|f| f.starts_with("ctime ")),
        "ctime is what proves the restore happened: {:?}",
        delta.facets
    );
    assert!(!outcome.artifacts_released());
    assert!(outcome.lease_quarantine_required());
}

// ─── contract semantics ─────────────────────────────────────────────────────

#[test]
fn declared_scope_accepts_in_scope_writes_and_rejects_the_rest() {
    // Creating `out/` changes the WORKSPACE ROOT directory's own fingerprint, and
    // on APFS that is not just mtime/ctime: a directory's `nlink` and `size` track
    // its child count. The ancestor-directory forgiveness therefore has to cover
    // the whole directory-bookkeeping facet set, or a declared write can never be
    // accepted at all (which is exactly how this test failed in round 1).
    let fx = Fixture::new();
    let clean = fx.run_worker(
        WriteContract::DeclaredScope {
            paths: vec!["out".to_string()],
        },
        |ws| {
            fs::create_dir_all(ws.join("out")).expect("mkdir out");
            fs::write(ws.join("out/result.json"), b"{}\n").expect("declared write");
        },
    );
    assert!(
        clean.artifacts_released(),
        "a write inside the declared scope must be accepted: {:?}",
        clean.verdict
    );

    let fx = Fixture::new();
    let rejected = fx.run_worker(
        WriteContract::DeclaredScope {
            paths: vec!["out".to_string()],
        },
        |ws| {
            fs::create_dir_all(ws.join("out")).expect("mkdir out");
            fs::write(ws.join("out/result.json"), b"{}\n").expect("declared write");
            fs::write(ws.join("src/tracked.rs"), b"fn main() { evil() }\n").expect("out of scope");
        },
    );
    assert_caught(&rejected, "src/tracked.rs", DeltaKind::ContentChanged);
    assert!(
        !rejected
            .deltas()
            .iter()
            .any(|d| d.path.contains("out/result.json")),
        "the declared write must not be reported as prohibited: {:#?}",
        rejected.deltas()
    );
}

#[cfg(unix)]
#[test]
fn declared_scope_forgives_only_bookkeeping_on_an_ancestor_dir_not_a_chmod() {
    // Guard the fix for the test above: the ancestor-directory forgiveness must
    // stay narrow. A worker that chmods a directory on the way to its declared
    // scope changed something the bookkeeping story does not explain, so the
    // gate must still reject — mode is not a directory-bookkeeping facet.
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new();
    let outcome = fx.run_worker(
        WriteContract::DeclaredScope {
            paths: vec!["src/out".to_string()],
        },
        |ws| {
            fs::create_dir_all(ws.join("src/out")).expect("mkdir");
            fs::write(ws.join("src/out/result.json"), b"{}\n").expect("declared write");
            // ...and, on the way past, widens the ancestor directory.
            fs::set_permissions(ws.join("src"), fs::Permissions::from_mode(0o777)).expect("chmod");
        },
    );
    assert_caught(&outcome, "workspace/src", DeltaKind::MetadataChanged);
    let delta = outcome
        .deltas()
        .iter()
        .find(|d| d.path == "workspace/src")
        .expect("the chmod'd ancestor");
    assert!(
        delta.facets.iter().any(|f| f.starts_with("mode ")),
        "the mode facet is what must defeat the bookkeeping forgiveness: {:?}",
        delta.facets
    );
}

#[test]
fn declared_scope_still_rejects_git_metadata_unless_declared() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(
        WriteContract::DeclaredScope {
            paths: vec!["src".to_string()],
        },
        |ws| {
            fs::write(ws.join("src/tracked.rs"), b"fn main() { ok() }\n").expect("in scope");
            fs::write(ws.join(".git/index"), b"binary-index-v2").expect("git metadata");
        },
    );
    assert_caught(&outcome, ".git/index", DeltaKind::ContentChanged);
}

// ─── ordering: descendants first (enforcement point 5) ──────────────────────

#[test]
fn a_live_descendant_blocks_the_gate_without_quarantine_or_release() {
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    gate.capture_preimage().expect("pre-image");
    fs::write(fx.ws().join("src/tracked.rs"), b"still writing\n").expect("mid-run write");

    let outcome = gate.run(&StillRunning).expect("gate run");
    let GateVerdict::Blocked { reason, .. } = &outcome.verdict else {
        panic!(
            "the gate must not decide while a descendant lives: {:?}",
            outcome.verdict
        );
    };
    assert_eq!(*reason, BlockReason::DescendantsAlive);
    // Nothing is released...
    assert!(!outcome.artifacts_released());
    assert!(outcome.release("patch").is_err());
    // ...and the lease is NOT quarantined/reclaimed while a process could still
    // be writing into it (that would race the writer).
    assert!(!outcome.lease_quarantine_required());
    let sink = RecordingSink::default();
    assert!(!apply_verdict(&outcome, &sink).expect("apply"));
    assert_eq!(sink.count(), 0);
}

#[test]
fn a_broken_liveness_probe_fails_closed() {
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    gate.capture_preimage().expect("pre-image");
    let err = gate
        .run(&ProbeBroken)
        .expect_err("an unanswerable liveness probe must never produce a clean verdict");
    assert!(err.contains("probe exploded"), "got: {err}");
}

#[cfg(unix)]
#[ignore = "issue #1261: spawn() returning != child process group immediately schedulable; on containerized CI runners kill(-pgid,0) can transiently return ESRCH in the scheduling window before the child's pgid is live. Run with --ignored"]
#[test]
fn process_group_liveness_sees_a_live_child_then_nothing_after_reap() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "sleep 30"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Same shape dispatch uses: the worker leads its own process group.
    cmd.process_group(0);
    let mut child = cmd.spawn().expect("spawn worker");

    let probe = super::ProcessGroupLiveness::for_worker_pid(child.id());
    assert!(
        probe.any_alive().expect("probe"),
        "a running worker must be visible to the liveness probe"
    );

    child.kill().expect("kill");
    child.wait().expect("reap");
    assert!(
        !probe.any_alive().expect("probe"),
        "after the worker is killed AND reaped the group must be empty"
    );
}

#[test]
fn process_group_liveness_refuses_the_daemons_own_group() {
    // kill(-0, 0) would probe the DAEMON's own process group and always answer
    // "alive"; kill(-1, 0) is a broadcast. Both must be an error, never a
    // silently wrong answer.
    for pid in [0u32, 1u32] {
        let probe = super::ProcessGroupLiveness::for_worker_pid(pid);
        assert!(
            probe.any_alive().is_err(),
            "pgid {pid} must be refused, not interpreted"
        );
    }
}

// ─── pre-image custody ──────────────────────────────────────────────────────

#[test]
fn a_preimage_inside_the_workspace_is_refused() {
    let fx = Fixture::new();
    let gate = PostflightGate::new(
        "env-test",
        fx.ws(),
        // The worker could rewrite this to match whatever it did.
        fx.ws().join(".tachi-preimage.json"),
        WriteContract::DetectAndReject,
    );
    let err = gate
        .capture_preimage()
        .expect_err("a worker-writable pre-image proves nothing");
    assert!(err.contains("inside the lease workspace"), "got: {err}");
}

#[test]
fn a_preimage_inside_the_external_gitdir_is_refused() {
    // The workspace is not the only root a same-UID worker can write: this module
    // declares TWO (the workspace and, for a linked worktree, the external
    // gitdir). A custody guard that only knows about the first one lets the
    // pre-image land in the second — where the worker can rewrite it to match
    // whatever it did, and the whole gate becomes the worker's own testimony.
    let fx = Fixture::linked_worktree();
    let gate = PostflightGate::new(
        "env-test",
        fx.ws(),
        fx.gitdir().join("tachi-preimage.json"),
        WriteContract::DetectAndReject,
    );
    let err = gate
        .capture_preimage()
        .expect_err("a pre-image inside the gitdir is as worker-writable as one inside the tree");
    assert!(err.contains("gitdir"), "got: {err}");
    assert!(
        err.contains("EVERY root the worker can write"),
        "got: {err}"
    );
    assert!(!fx.gitdir().join("tachi-preimage.json").exists());
}

// ─── capture cost: unhashed build dirs are disclosed, never assumed unchanged ──

#[test]
fn unhashed_build_dirs_are_still_detected_and_are_named_on_the_receipt() {
    // Hashing every file includes an in-tree Rust `target/` — multiple GB of
    // BLAKE2 per capture, twice per dispatch. Opting that subtree out of HASHING
    // must not opt it out of DETECTION: it is still walked and fingerprinted
    // (size/inode/nlink/mode/xattr/mtime/ctime), so a same-size tamper — the
    // hardest case for a metadata-only proof — is still caught by mtime/ctime.
    // And the receipt says, by name, which paths carry only that weaker proof.
    let fx = Fixture::new();
    fs::create_dir_all(fx.ws().join("target/debug")).expect("target dir");
    fs::write(fx.ws().join("target/debug/artifact.bin"), b"aaaaaaaa").expect("artifact");

    let gate = fx
        .gate(WriteContract::DetectAndReject)
        .with_build_artifacts_unhashed();
    let pre = gate.capture_preimage().expect("pre-image");

    // The expensive half really was skipped — and ONLY there.
    assert_eq!(pre.unhashed_roots, vec!["workspace/target".to_string()]);
    assert!(
        pre.entries["workspace/target/debug/artifact.bin"]
            .content_hash
            .is_none(),
        "the build artifact must not have been hashed"
    );
    assert!(
        pre.entries["workspace/src/tracked.rs"]
            .content_hash
            .is_some(),
        "everything outside the build dir is still hashed"
    );

    // A same-size overwrite: no size facet, no content hash to compare — only the
    // timestamps testify, and the sealed barrier is what makes them able to.
    fs::write(fx.ws().join("target/debug/artifact.bin"), b"bbbbbbbb").expect("tamper");

    let outcome = gate.run(&Reaped).expect("gate run");
    assert_caught(
        &outcome,
        "target/debug/artifact.bin",
        DeltaKind::MetadataChanged,
    );

    let receipt = outcome.receipt();
    assert_eq!(receipt["content_unhashed_paths"][0], "workspace/target");
    let note = receipt["content_unhashed_note"]
        .as_str()
        .expect("the receipt must state which paths were not hashed");
    assert!(note.contains("NOT hashed"), "got: {note}");
    assert!(note.contains("workspace/target"), "got: {note}");
}

#[test]
fn a_clean_verdict_over_unhashed_dirs_carries_the_caveat_on_every_surface() {
    // "I did not hash it" must never be silently read as "it did not change" —
    // so a CLEAN run over an unhashed subtree still says so, in the receipt and
    // on the log line, not only when something goes wrong.
    let fx = Fixture::new();
    fs::create_dir_all(fx.ws().join("target")).expect("target dir");
    fs::write(fx.ws().join("target/artifact.bin"), b"cached").expect("artifact");

    let gate = fx
        .gate(WriteContract::DetectAndReject)
        .with_build_artifacts_unhashed();
    gate.capture_preimage().expect("pre-image");
    let outcome = gate.run(&Reaped).expect("gate run");

    assert!(outcome.artifacts_released(), "{:?}", outcome.verdict);
    assert_eq!(
        outcome.content_unhashed_paths,
        vec!["workspace/target".to_string()]
    );
    let log = rejection_log_message(&outcome);
    assert!(
        log.contains("NOT hashed"),
        "the pass line hides its asterisk: {log}"
    );
    assert!(naming::violates_naming_rule(&log).is_none());
    assert!(outcome.receipt()["content_unhashed_note"].is_string());
}

#[test]
fn hashing_everything_is_the_default_and_leaves_no_caveat() {
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    let pre = gate.capture_preimage().expect("pre-image");
    assert!(pre.unhashed_roots.is_empty());
    assert!(pre.entries["workspace/ignored/build.log"]
        .content_hash
        .is_some());

    let outcome = gate.run(&Reaped).expect("gate run");
    assert!(outcome.content_unhashed_paths.is_empty());
    assert!(outcome.unhashed_caveat().is_none());
    assert!(outcome.receipt()["content_unhashed_note"].is_null());
}

#[cfg(unix)]
#[test]
fn an_unreadable_entry_makes_the_preimage_fail_closed() {
    use std::os::unix::fs::PermissionsExt;

    // SAFETY: `geteuid()` reads the caller's effective uid; no pointers, no
    // aliasing. Running as root would defeat the chmod, so skip there.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let fx = Fixture::new();
    let locked = fx.ws().join("locked");
    fs::create_dir_all(&locked).expect("mkdir");
    fs::write(locked.join("secret"), b"x").expect("file");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let gate = fx.gate(WriteContract::DetectAndReject);
    let err = gate
        .capture_preimage()
        .expect_err("a workspace the parent cannot fully read cannot be proven unchanged");
    assert!(err.contains("INCOMPLETE"), "got: {err}");
    // And nothing was written that could later be mistaken for a valid pre-image.
    assert!(!fx.preimage_path().exists());

    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("restore");
}

#[test]
fn a_missing_preimage_fails_closed_it_is_not_a_pass() {
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    // No capture_preimage() call at all.
    let outcome = gate.run(&Reaped).expect("gate run");
    let GateVerdict::Rejected { reason, .. } = &outcome.verdict else {
        panic!(
            "no pre-image means nothing can be proven: {:?}",
            outcome.verdict
        );
    };
    assert_eq!(*reason, RejectReason::UnusableImage);
    assert!(!outcome.artifacts_released());
    assert!(outcome.lease_quarantine_required());
}

// ─── quarantine (evidence, not reclaim) ─────────────────────────────────────

#[derive(Default)]
struct RecordingSink {
    calls: std::sync::Mutex<Vec<String>>,
}
impl RecordingSink {
    fn count(&self) -> usize {
        self.calls.lock().expect("lock").len()
    }
}
impl QuarantineSink for RecordingSink {
    fn quarantine(&self, outcome: &GateOutcome) -> Result<(), String> {
        self.calls
            .lock()
            .expect("lock")
            .push(outcome.env_id.clone());
        Ok(())
    }
}

#[test]
fn rejection_quarantines_the_lease_and_writes_a_forensic_receipt() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        fs::write(ws.join("src/tracked.rs"), b"tampered\n").expect("edit");
    });

    let sink = FileQuarantineSink {
        dir: fx.quarantine_dir(),
    };
    assert!(apply_verdict(&outcome, &sink).expect("quarantine"));

    let receipts: Vec<_> = fs::read_dir(fx.quarantine_dir())
        .expect("quarantine dir")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(receipts.len(), 1, "one forensic receipt per rejected lease");
    let body = fs::read_to_string(receipts[0].path()).expect("receipt body");
    let receipt: serde_json::Value = serde_json::from_str(&body).expect("receipt json");
    assert_eq!(receipt["verdict"], "rejected");
    assert_eq!(receipt["artifacts"], "withheld");
    assert_eq!(receipt["lease_action"], "quarantined");
    assert!(receipt["prohibited_deltas"]
        .as_array()
        .expect("deltas array")
        .iter()
        .any(
            |d| d["path"].as_str().unwrap_or("").ends_with("src/tracked.rs")
                && d["kind"] == "content_changed"
        ));

    // Quarantine preserves the evidence — it is NOT a reclaim.
    assert!(
        fx.ws().join("src/tracked.rs").exists(),
        "quarantine must not delete the workspace it is preserving as evidence"
    );
}

#[test]
fn a_clean_verdict_quarantines_nothing() {
    let fx = Fixture::new();
    let outcome = fx.run_worker(WriteContract::DetectAndReject, |_ws| {});
    let sink = RecordingSink::default();
    assert!(!apply_verdict(&outcome, &sink).expect("apply"));
    assert_eq!(sink.count(), 0);
}

// ─── naming discipline (the frozen invariant) ───────────────────────────────

#[test]
fn naming_discipline_no_surface_markets_this_as_read_only_enforcement() {
    let fx = Fixture::new();

    let rejected = fx.run_worker(WriteContract::DetectAndReject, |ws| {
        fs::write(ws.join("src/tracked.rs"), b"tampered\n").expect("edit");
    });
    let fx_clean = Fixture::new();
    let clean = fx_clean.run_worker(WriteContract::DetectAndReject, |_ws| {});
    let gate = fx.gate(WriteContract::DetectAndReject);
    let blocked = gate.run(&StillRunning).expect("blocked run");

    let mut surfaces: Vec<String> = Vec::new();
    for outcome in [&rejected, &clean, &blocked] {
        surfaces.push(outcome.receipt().to_string());
        surfaces.push(outcome.trajectory_event().to_string());
        surfaces.push(rejection_log_message(outcome));
        if let Some(message) = outcome.failure_message() {
            surfaces.push(message);
        }
        if let Err(withheld) = outcome.release("patch") {
            surfaces.push(withheld);
        }
    }

    for surface in &surfaces {
        if let Some(phrase) = naming::violates_naming_rule(surface) {
            panic!(
                "this gate must never be presented as read-only enforcement — found {phrase:?} in:\n{surface}"
            );
        }
    }

    // ...and it must state, positively, what it does and does not prove.
    let receipt = rejected.receipt();
    assert_eq!(receipt["posture"], naming::POSTURE);
    assert_eq!(receipt["proves"], naming::PROVES);
    assert!(receipt["does_not_prove"]
        .as_str()
        .expect("does_not_prove")
        .contains("no change occurred"));
    let message = rejected.failure_message().expect("message");
    assert!(message.contains("detect-and-reject"), "got: {message}");
    assert!(
        message.contains("no change was accepted"),
        "the failure message must state what the gate actually proves: {message}"
    );
}

#[test]
fn naming_rule_detector_actually_detects() {
    // Guard the guard: if `violates_naming_rule` stopped matching, every
    // naming-discipline assertion above would pass vacuously.
    assert_eq!(
        naming::violates_naming_rule("Tachi applied READ-ONLY ENFORCEMENT to the lease"),
        Some("read-only enforcement")
    );
    assert!(naming::violates_naming_rule("detect-and-reject postflight gate").is_none());
}

// ─── the capture-time clock barrier (#1440) ─────────────────────────────────
//
// Two of the eight surfaces above — mutate-then-restore, and the same-size
// overwrite under an unhashed root — have NO witness except a timestamp. Every
// other fingerprint field is equal by construction there (measured: a same-size
// in-place rewrite moves only {mtime, ctime}; mutate-then-restore + utimensat
// moves only {ctime}). So those two tests are decided entirely by whether the
// filesystem clock ticked between the capture and the write — and on a coarse
// clock (a Linux jiffy, 1–4 ms) it often does not, at which point the gate saw
// nothing and certified a mutated tree as Clean. That is not a flaky assertion;
// it is a fail-open, and it is why those two tests were red 1-in-3 on Linux.
//
// The mechanism fix is the barrier below. These tests pin it from both ends: the
// barrier is really established (and really dominates), and an image WITHOUT a
// dominating barrier is refused instead of passed.

/// Rewrite the on-disk pre-image. Used to put the gate in the exact state a
/// coarse clock — or an older binary — would leave it in, without touching the
/// workspace at all, so the only thing under test is what the gate does with an
/// image it cannot trust.
fn rewrite_preimage(
    path: &Path,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) {
    let bytes = fs::read(path).expect("read pre-image");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse pre-image");
    edit(value.as_object_mut().expect("pre-image is a JSON object"));
    fs::write(
        path,
        serde_json::to_vec(&value).expect("serialize pre-image"),
    )
    .expect("write pre-image");
}

#[test]
fn the_preimage_is_sealed_with_an_observed_clock_barrier_past_every_ctime() {
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    let pre = gate.capture_preimage().expect("pre-image");

    let barrier = *pre
        .clock_barriers
        .get("workspace")
        .expect("a sealed pre-image must carry a barrier for the workspace walk root");
    for (key, entry) in &pre.entries {
        let ctime = FsTime::new(entry.ctime_sec, entry.ctime_nsec);
        assert!(
            barrier > ctime,
            "the barrier {barrier} must STRICTLY exceed the ctime {ctime} recorded at {key}, or a \
             write in that same tick is invisible"
        );
    }
    pre.verify_clock_barriers()
        .expect("sealing and verifying must agree, or the barrier is decorative");
}

#[cfg(unix)]
#[test]
fn a_write_after_the_barrier_is_stamped_strictly_past_the_recorded_ctime() {
    // The property the whole gate rests on, asserted directly: once the
    // pre-image is sealed, ANY inode change carries a ctime at or past the
    // barrier — hence strictly past what the pre-image recorded — so
    // `compare_entry` cannot come back empty for a mutated entry no matter how
    // coarse the filesystem clock is. This is the assertion that goes red on a
    // 1–4 ms tick if the barrier is ever removed or weakened.
    use std::os::unix::fs::MetadataExt;

    let fx = Fixture::new();
    let target = fx.ws().join("src/tracked.rs");
    let gate = fx.gate(WriteContract::DetectAndReject);
    let pre = gate.capture_preimage().expect("pre-image");

    let recorded = &pre.entries["workspace/src/tracked.rs"];
    let recorded_ctime = FsTime::new(recorded.ctime_sec, recorded.ctime_nsec);
    let barrier = *pre.clock_barriers.get("workspace").expect("barrier");

    // The worker writes the instant the barrier is in place: the tightest
    // window there is, and the one a same-tick collision needs.
    fs::write(&target, b"// tamper\n").expect("mutate");
    let after = fs::symlink_metadata(&target).expect("metadata");
    let after_ctime = FsTime::new(after.ctime(), after.ctime_nsec());

    assert!(
        after_ctime >= barrier,
        "the filesystem clock is not monotone with the observed barrier: wrote at {after_ctime}, \
         barrier was {barrier}"
    );
    assert!(
        after_ctime > recorded_ctime,
        "a post-capture write must be stamped strictly past the pre-image's ctime; got \
         {after_ctime} vs recorded {recorded_ctime} (barrier {barrier})"
    );
}

#[test]
fn a_preimage_with_no_clock_barrier_is_not_a_pass() {
    // Byte-for-byte the pre-image an older binary wrote — and the state any
    // capture is in when the clock advance could not be observed. Nothing in
    // the workspace is touched, so the diff is empty and the ONLY question is
    // what the gate does with an image whose timestamps prove nothing.
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    gate.capture_preimage().expect("pre-image");

    rewrite_preimage(&fx.preimage_path(), |obj| {
        obj.remove("clock_barriers");
    });

    let outcome = gate.run(&Reaped).expect("gate run");
    let GateVerdict::Rejected { reason, deltas, .. } = &outcome.verdict else {
        panic!(
            "an unsealed pre-image cannot prove a match means unchanged; passing it as clean is \
             the #1440 fail-open: {:?}",
            outcome.verdict
        );
    };
    assert_eq!(*reason, RejectReason::UnusableImage);
    assert!(
        deltas
            .iter()
            .any(|d| d.detail.contains("clock barrier") && d.kind == DeltaKind::Unreadable),
        "the receipt must say WHY it could not decide: {deltas:#?}"
    );
    assert!(!outcome.artifacts_released());
    assert!(outcome.lease_quarantine_required());
}

#[test]
fn a_clock_barrier_that_does_not_outrank_the_recorded_ctimes_is_not_a_pass() {
    // The coarse-clock filesystem, reproduced at the one place it is observable
    // from a test: an image sealed with a barrier that does NOT strictly exceed
    // what the walk recorded. That is exactly the state a 1–4 ms tick leaves the
    // capture in, and the gate must refuse it rather than read "every field
    // matched" as "nothing happened".
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    gate.capture_preimage().expect("pre-image");

    rewrite_preimage(&fx.preimage_path(), |obj| {
        obj.insert(
            "clock_barriers".to_string(),
            serde_json::json!({ "workspace": { "sec": 0, "nsec": 0 } }),
        );
    });

    let outcome = gate.run(&Reaped).expect("gate run");
    let GateVerdict::Rejected { reason, .. } = &outcome.verdict else {
        panic!(
            "a barrier that does not dominate the recorded ctimes proves nothing; passing it as \
             clean is the #1440 fail-open: {:?}",
            outcome.verdict
        );
    };
    assert_eq!(*reason, RejectReason::UnusableImage);
    assert!(!outcome.artifacts_released());
    assert!(outcome.lease_quarantine_required());
}

#[test]
fn a_linked_worktrees_gitdir_gets_its_own_barrier() {
    // A barrier measured on one filesystem says nothing about another's clock,
    // so every walk root carries its own — including the external gitdir, which
    // routinely lives on a different mount from the lease.
    let fx = Fixture::linked_worktree();
    let gate = fx.gate(WriteContract::DetectAndReject);
    let pre = gate.capture_preimage().expect("pre-image");

    for label in ["workspace", "gitdir"] {
        assert!(
            pre.clock_barriers.contains_key(label),
            "walk root {label} has entries but no barrier: {:?}",
            pre.clock_barriers
        );
    }
    pre.verify_clock_barriers().expect("both roots are sealed");

    // And a barrier for one root does not vouch for the other.
    rewrite_preimage(&fx.preimage_path(), |obj| {
        if let Some(barriers) = obj
            .get_mut("clock_barriers")
            .and_then(|b| b.as_object_mut())
        {
            barriers.remove("gitdir");
        }
    });
    let outcome = gate.run(&Reaped).expect("gate run");
    let GateVerdict::Rejected { reason, .. } = &outcome.verdict else {
        panic!(
            "an unsealed gitdir root must not ride in on the workspace's barrier: {:?}",
            outcome.verdict
        );
    };
    assert_eq!(*reason, RejectReason::UnusableImage);
}

#[test]
fn the_clock_probe_is_written_outside_every_walk_root() {
    // The probe cannot live inside the workspace: an entry the walk already
    // fingerprinted cannot be the witness (bumping it bumps the value the
    // barrier must exceed — the chase never converges), and the parent writing
    // into the tree it is about to certify would change this gate's custody
    // story. So a sealed capture must leave the workspace byte-identical and
    // the following clean run must still pass.
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    let pre = gate.capture_preimage().expect("pre-image");

    assert!(
        !pre.entries.keys().any(|key| key.contains("clock-probe")),
        "the clock probe leaked into the manifest: {:?}",
        pre.entries.keys().collect::<Vec<_>>()
    );
    for entry in fs::read_dir(fx.ws()).expect("read workspace") {
        let name = entry.expect("dir entry").file_name();
        assert!(
            !name.to_string_lossy().contains("clock-probe"),
            "the clock probe was left inside the lease workspace: {name:?}"
        );
    }

    // Sealing must not itself be a delta.
    let outcome = gate.run(&Reaped).expect("gate run");
    assert!(
        matches!(outcome.verdict, GateVerdict::Clean { .. }),
        "sealing the pre-image must not show up as a change: {:?}",
        outcome.verdict
    );
}

// ─── the barrier's two preconditions (#1440 review) ─────────────────────────
//
// A barrier is the argument "I watched this filesystem's clock pass X, so
// anything it stamps later is > X". That argument needs the clock it measured to
// be the clock that stamps the entries, and needs that clock to move when an
// inode changes. Neither is universal. Both are recorded IN THE IMAGE — which is
// what makes them testable here rather than only on the platform that lacks
// them: the tests below put the gate in the state each precondition failure
// produces and assert it refuses, exactly as the barrier-absent test above does.

#[test]
fn a_sealed_preimage_names_the_ctime_witness_it_actually_used() {
    // The positive half. On unix the witness is POSIX ctime, and the gate is
    // willing to run. If this ever records `None` on a platform where the gate
    // still passes, the refusal below has been disconnected.
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    let pre = gate.capture_preimage().expect("pre-image");

    assert_eq!(
        pre.ctime_witness,
        CtimeWitness::PosixCtime,
        "a unix capture must record the POSIX ctime witness, or its ctime fields prove nothing"
    );
    pre.verify_timestamp_preconditions()
        .expect("a single-filesystem unix capture satisfies both preconditions");
    assert!(
        pre.foreign_device_paths.is_empty(),
        "a single-filesystem workspace must not report foreign devices: {:?}",
        pre.foreign_device_paths
    );
}

#[test]
fn an_image_with_no_ctime_witness_is_not_a_pass() {
    // The platform hole this review caught: `#[cfg(not(unix))] unix_ctime` used
    // to return `created()`, which does NOT advance when a file's contents
    // change — so the barrier spin could "observe" an advance in a field that
    // can never move and MANUFACTURE a proof. That is worse than the original
    // fail-open: the original lost a signal, this one invents one.
    //
    // The refusal is data-driven precisely so it can be exercised from a unix
    // test: this is byte-for-byte the pre-image such a platform produces.
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    gate.capture_preimage().expect("pre-image");

    rewrite_preimage(&fx.preimage_path(), |obj| {
        obj.insert(
            "ctime_witness".to_string(),
            serde_json::Value::String("none".to_string()),
        );
    });

    let outcome = gate.run(&Reaped).expect("gate run");
    let GateVerdict::Rejected { reason, deltas, .. } = &outcome.verdict else {
        panic!(
            "an image whose ctime field cannot advance proves nothing about mutate-then-restore; \
             passing it as clean manufactures a proof: {:?}",
            outcome.verdict
        );
    };
    assert_eq!(*reason, RejectReason::UnusableImage);
    assert!(
        deltas.iter().any(|d| d.detail.contains("ctime witness")),
        "the receipt must name the missing witness: {deltas:#?}"
    );
    assert!(!outcome.artifacts_released());
}

#[test]
fn an_absent_ctime_witness_field_reads_as_none_not_as_a_guarantee() {
    // An older binary wrote no `ctime_witness` at all. `#[serde(default)]` must
    // land on the pessimistic value — a field that is missing must not default
    // into the guarantee the image never carried.
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    gate.capture_preimage().expect("pre-image");

    rewrite_preimage(&fx.preimage_path(), |obj| {
        obj.remove("ctime_witness");
    });

    let outcome = gate.run(&Reaped).expect("gate run");
    assert!(
        matches!(
            outcome.verdict,
            GateVerdict::Rejected {
                reason: RejectReason::UnusableImage,
                ..
            }
        ),
        "a missing ctime_witness must fail closed, not default to posix_ctime: {:?}",
        outcome.verdict
    );
}

#[test]
fn a_walk_root_spanning_two_filesystems_is_not_a_pass() {
    // The barrier is measured with a probe on the ROOT's device. An entry under
    // the root but on another mount is stamped by a clock the probe never
    // watched — the same cross-filesystem claim `establish_clock_barrier`
    // already refuses when the PROBE lands on the wrong device, left open one
    // level down until now.
    //
    // Creating a real second mount inside a temp dir needs privileges no test
    // has, so this drives the refusal from the recorded state rather than from a
    // live mount; `shares_barrier_device` below covers the decision that
    // populates it.
    let fx = Fixture::new();
    let gate = fx.gate(WriteContract::DetectAndReject);
    gate.capture_preimage().expect("pre-image");

    rewrite_preimage(&fx.preimage_path(), |obj| {
        obj.insert(
            "foreign_device_paths".to_string(),
            serde_json::json!(["workspace/src (device 42; walk root workspace is device 7)"]),
        );
    });

    let outcome = gate.run(&Reaped).expect("gate run");
    let GateVerdict::Rejected { reason, deltas, .. } = &outcome.verdict else {
        panic!(
            "a barrier proven on one filesystem must not certify entries on another: {:?}",
            outcome.verdict
        );
    };
    assert_eq!(*reason, RejectReason::UnusableImage);
    assert!(
        deltas
            .iter()
            .any(|d| d.detail.contains("more than one filesystem")),
        "the receipt must say the walk root spans filesystems: {deltas:#?}"
    );
    assert!(!outcome.artifacts_released());
}

#[test]
fn an_unknown_device_is_not_treated_as_the_same_device() {
    // The one-line fail-open this guards: `root_dev == entry_dev` on two
    // `Option<u64>`s makes `None == None` TRUE, so a platform that reports no
    // device id at all would have every entry qualify as "same filesystem as the
    // root" and inherit a barrier that never measured it. Unknown is not a
    // match, in either direction.
    assert!(manifest::shares_barrier_device(Some(7), Some(7)));
    assert!(!manifest::shares_barrier_device(Some(7), Some(42)));
    assert!(!manifest::shares_barrier_device(Some(7), None));
    assert!(!manifest::shares_barrier_device(None, Some(7)));
    assert!(
        !manifest::shares_barrier_device(None, None),
        "two unknown device ids are not evidence of one filesystem; treating them as equal is the \
         fail-open"
    );
}

#[test]
fn the_receipt_states_what_was_established_not_what_is_generally_true() {
    // The prose half. A clean run may report the barrier it verified; a run that
    // verified nothing must NOT read like one that did. The old caveat asserted
    // "ctime cannot be restored by an unprivileged worker" and "the pre-image is
    // sealed with an observed capture-time clock barrier" on every run, from a
    // struct that did not know either — the confident-claim shape this issue
    // exists to stop.
    let fx = Fixture::new();
    let gate = fx
        .gate(WriteContract::DetectAndReject)
        .with_build_artifacts_unhashed();
    fs::create_dir_all(fx.ws().join("target")).expect("target");
    fs::write(fx.ws().join("target/artifact.bin"), b"aaaa").expect("artifact");
    gate.capture_preimage().expect("pre-image");

    let clean = gate.run(&Reaped).expect("gate run");
    assert!(matches!(clean.verdict, GateVerdict::Clean { .. }));
    let ClockBarrierEvidence::Verified(roots) = &clean.clock_barrier else {
        panic!(
            "a run that passed the barrier check must report what it verified: {:?}",
            clean.clock_barrier
        );
    };
    assert!(
        roots.contains_key("workspace"),
        "the evidence must name the roots it covers: {roots:?}"
    );
    let caveat = clean
        .unhashed_caveat()
        .expect("unhashed paths carry a caveat");
    assert!(
        caveat.contains("This run verified"),
        "the caveat must report a measurement, not a general property: {caveat}"
    );
    assert!(
        !caveat.contains("cannot be restored by an unprivileged worker"),
        "the caveat re-committed the unconditional guarantee it was rewritten to drop: {caveat}"
    );
    assert_eq!(clean.receipt()["clock_barrier"]["status"], "verified");

    // …and the same surface on a run that established nothing: no pre-image at
    // all, so the barrier was never reached.
    let fresh = Fixture::new();
    let ungated = fresh
        .gate(WriteContract::DetectAndReject)
        .with_build_artifacts_unhashed();
    let outcome = ungated.run(&Reaped).expect("gate run");
    assert_eq!(
        outcome.clock_barrier,
        ClockBarrierEvidence::NotEstablished,
        "a run that never read a pre-image cannot claim a verified barrier"
    );
    assert!(
        outcome
            .clock_barrier
            .describe()
            .contains("did NOT establish"),
        "an unestablished barrier must say so: {}",
        outcome.clock_barrier.describe()
    );
    assert_eq!(
        outcome.receipt()["clock_barrier"]["status"],
        "not_established"
    );
}

// ─── unix test helpers ──────────────────────────────────────────────────────

#[cfg(unix)]
fn access_and_modify_times(path: &Path) -> ((i64, i64), (i64, i64)) {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(path).expect("metadata");
    (
        (meta.atime(), meta.atime_nsec()),
        (meta.mtime(), meta.mtime_nsec()),
    )
}

/// Put the original atime/mtime back **to the nanosecond** — an unprivileged
/// worker can do exactly this (`utimensat`), which is why the gate does not rely
/// on mtime. ctime cannot be set this way by anyone but root.
#[cfg(unix)]
fn set_times(path: &Path, atime: (i64, i64), mtime: (i64, i64)) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).expect("path");
    let times = [
        libc::timespec {
            tv_sec: atime.0 as libc::time_t,
            tv_nsec: atime.1 as _,
        },
        libc::timespec {
            tv_sec: mtime.0 as libc::time_t,
            tv_nsec: mtime.1 as _,
        },
    ];
    // SAFETY: `utimensat` takes a NUL-terminated path from a live CString and a
    // two-element timespec array, exactly as declared; no Rust memory is aliased.
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(
        rc,
        0,
        "utimensat failed: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn set_xattr(path: &Path, name: &str, value: &[u8]) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).expect("path");
    let c_name = CString::new(name).expect("name");
    // SAFETY: both pointers come from live CStrings; `value` is a live slice
    // whose length is passed as the size argument (symlinks are not followed).
    let rc = unsafe {
        set_xattr_raw(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr() as *const libc::c_void,
            value.len(),
        )
    };
    assert_eq!(
        rc,
        0,
        "setxattr failed: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe fn set_xattr_raw(
    path: *const libc::c_char,
    name: *const libc::c_char,
    value: *const libc::c_void,
    size: usize,
) -> libc::c_int {
    libc::setxattr(path, name, value, size, 0, libc::XATTR_NOFOLLOW)
}

#[cfg(target_os = "linux")]
unsafe fn set_xattr_raw(
    path: *const libc::c_char,
    name: *const libc::c_char,
    value: *const libc::c_void,
    size: usize,
) -> libc::c_int {
    libc::lsetxattr(path, name, value, size, 0)
}
