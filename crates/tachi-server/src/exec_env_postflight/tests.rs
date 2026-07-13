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
use super::manifest::DeltaKind;
use super::{
    apply_verdict, naming, rejection_log_message, BlockReason, FileQuarantineSink, GateOutcome,
    GateVerdict, PostflightGate, QuarantineSink, RejectReason, WriteContract,
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
    // tool would look at says "unchanged". Only ctime, which no unprivileged
    // process can set, still testifies.
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
    // timestamps testify, and ctime cannot be put back by an unprivileged worker.
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
