//! `detect-and-reject` postflight gate (#894 S2e) — ordered enforcement points
//! 4 (parent-owned postflight manifest/diff gate) and 5 (quarantine/reclaim
//! only after descendants terminate).
//!
//! # ⚠ NOT WIRED YET — see #894 S2 wiring slice
//!
//! **This gate is a mechanism with no callers.** Nothing in the dispatch path
//! calls [`PostflightGate::capture_preimage`], [`PostflightGate::run`] or
//! [`apply_verdict`] today, so **zero dispatches are currently gated by it** —
//! landing this module changed the enforcement posture of exactly nothing.
//! Wiring it into lease provisioning / teardown (capture before spawn, run after
//! the worker is reaped, release artifacts only on a clean verdict) is the S2c /
//! dispatch-surface slice. Do not read the tests below as evidence that any live
//! dispatch is protected.
//!
//! # The name is the contract
//!
//! Owner ratification (2026-07-13, sol round `codex-e0255`): with an
//! unrestricted **same-UID shell**, Tachi **cannot stop a worker from writing**.
//! So for any dispatch that has no *qualified preventive provider*, the honest
//! second-best posture is this one, and it must be named honestly:
//!
//! * a disposable managed lease,
//! * a **complete pre-image** of the lease workspace, captured **by the parent,
//!   before the worker is spawned**, and stored where the worker cannot reach it,
//! * after the worker **and every descendant** have terminated: a re-scan, a
//!   comparison against that pre-image, and — on any prohibited delta — a
//!   **loud failure**, a **quarantined** lease, and **no patch / no result**.
//!
//! It proves **"no change was accepted."** It does **not** prove "no change
//! occurred." It is **never** an enforcement of a preventive read-only
//! contract, and presenting it as one is a defect, not a wording preference
//! (frozen invariant: *`detect-and-reject` never satisfies a preventive
//! read-only requirement*). The [`naming`] module holds the phrases that may
//! never appear on this gate's surfaces, and `naming_discipline_*` tests assert
//! every receipt, error and log line against them.
//!
//! # Why a `git diff` is not the mechanism
//!
//! `git diff` is blind to ignored files, git metadata, xattrs, and
//! mutate-then-restore. This gate walks the workspace itself — see
//! [`manifest`], which fingerprints content (BLAKE2s-256), symlink targets,
//! xattrs, mode/size/nlink, inode, mtime **and ctime**.
//!
//! Two of those classes (mutate-then-restore; a same-size overwrite under an
//! unhashed root) have **no witness except a timestamp**, so the pre-image is
//! additionally **sealed with an observed capture-time clock barrier** before a
//! worker is spawned, and [`PostflightGate::run`] refuses any pre-image that
//! does not carry one. Without that, a write landing inside the capture's own
//! clock tick leaves every field equal and the gate certifies a mutated tree as
//! clean — see the `manifest` module docs and #1440.
//!
//! That barrier has preconditions (a POSIX ctime witness; one filesystem per
//! walk root), each of which is a **refusal** rather than a downgrade, and each
//! run reports what it actually established in
//! [`GateOutcome::clock_barrier`] — a run that verified nothing must not read
//! like a run that verified something.
//!
//! # Composition (what this module does NOT decide)
//!
//! *Which* contract a dispatch runs under is the job of the effective-authority
//! compiler (enforcement point 1, a separate slice). This module is the
//! mechanism: give it a lease workspace, a [`WriteContract`], and a liveness
//! probe, and it returns a verdict plus the receipt, and it withholds the
//! artifacts when the verdict is not clean.

pub mod liveness;
pub mod manifest;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub use liveness::{DescendantLiveness, ProcessGroupLiveness};
pub use manifest::{
    CaptureSpec, CtimeWitness, DeltaKind, FsTime, WorkspaceDelta, WorkspaceManifest,
};

/// The vocabulary this gate is allowed (and forbidden) to describe itself with.
pub mod naming {
    /// The one honest label.
    pub const POSTURE: &str = "detect-and-reject";

    /// What a clean verdict actually proves.
    pub const PROVES: &str = "no change was accepted";

    /// What it does NOT prove — carried on every receipt so a reader can never
    /// mistake a clean verdict for prevention.
    pub const DOES_NOT_PROVE: &str = "no change occurred: a same-UID worker with a shell can still write to the workspace; this gate is post-hoc detection, and it never satisfies a preventive read-only requirement";

    /// Phrases that must never appear on a surface this gate produces
    /// (receipt / error / log line). Asserted by the `naming_discipline_*`
    /// tests: this gate may not be marketed as read-only enforcement.
    pub const FORBIDDEN_MARKETING: &[&str] = &[
        "read-only enforcement",
        "read only enforcement",
        "readonly enforcement",
        "read_only_enforcement",
        "enforces read-only",
        "enforced read-only",
        "read-only enforced",
        "read-only sandbox",
        "prevented the write",
        "prevents writes",
        "write prevention",
    ];

    /// Does `text` market this mechanism as something it is not?
    pub fn violates_naming_rule(text: &str) -> Option<&'static str> {
        let haystack = text.to_ascii_lowercase();
        FORBIDDEN_MARKETING
            .iter()
            .copied()
            .find(|phrase| haystack.contains(*phrase))
    }
}

/// What the lease's occupant was allowed to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteContract {
    /// Nothing in the workspace may change. Any delta at all is prohibited.
    ///
    /// Note the consequence, which is deliberate: even a benign `git status`
    /// index refresh writes `gitdir/index` and therefore trips this gate. The
    /// contract is about what the parent is willing to *accept*, and a state
    /// change it cannot account for is not acceptable. A caller that wants to
    /// tolerate a specific benign surface must declare it — see
    /// [`WriteContract::DeclaredScope`] — rather than have the gate quietly
    /// forgive a class of writes.
    DetectAndReject,

    /// Only changes under these declared paths may be accepted; everything else
    /// (including git metadata, unless declared) is a prohibited delta.
    ///
    /// Entries are manifest keys: bare entries are workspace-relative
    /// (`src/foo.rs` → `workspace/src/foo.rs`); git metadata is declared
    /// explicitly as `gitdir` or `gitdir/<path>`.
    DeclaredScope { paths: Vec<String> },
}

impl WriteContract {
    pub fn label(&self) -> &'static str {
        match self {
            WriteContract::DetectAndReject => "detect_and_reject",
            WriteContract::DeclaredScope { .. } => "declared_scope",
        }
    }

    /// Is this delta inside what the caller declared? Under
    /// [`WriteContract::DetectAndReject`] the answer is always no — nothing is
    /// forgiven, ever.
    pub fn permits(&self, delta: &WorkspaceDelta) -> bool {
        match self {
            WriteContract::DetectAndReject => false,
            WriteContract::DeclaredScope { paths } => {
                if paths
                    .iter()
                    .any(|declared| scope_covers(declared, &delta.path))
                {
                    return true;
                }
                // A directory on the path to a declared write records that write
                // in its own entry-count bookkeeping (mtime/ctime, and — on
                // APFS — nlink/size too) and in nothing else. Forgiving exactly
                // that, and only on a directory that is an ancestor of a declared
                // path, is what keeps a declared scope usable. A chmod, an xattr,
                // an inode swap or a content change on the same directory still
                // rejects, and the child that caused the bump is itself a delta
                // that must be permitted on its own.
                delta.is_dir_bookkeeping_metadata()
                    && paths
                        .iter()
                        .any(|declared| key_is_ancestor_of_declared(&delta.path, declared))
            }
        }
    }

    fn declared_paths(&self) -> Vec<String> {
        match self {
            WriteContract::DetectAndReject => Vec::new(),
            WriteContract::DeclaredScope { paths } => {
                paths.iter().map(|p| normalize_scope_entry(p)).collect()
            }
        }
    }
}

/// Normalize a declared scope entry into the manifest key namespace.
fn normalize_scope_entry(entry: &str) -> String {
    let trimmed = entry.trim().trim_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return manifest::WORKSPACE_ROOT_LABEL.to_string();
    }
    if trimmed == manifest::GITDIR_ROOT_LABEL
        || trimmed.starts_with(&format!("{}/", manifest::GITDIR_ROOT_LABEL))
        || trimmed == manifest::WORKSPACE_ROOT_LABEL
        || trimmed.starts_with(&format!("{}/", manifest::WORKSPACE_ROOT_LABEL))
    {
        return trimmed.to_string();
    }
    format!("{}/{trimmed}", manifest::WORKSPACE_ROOT_LABEL)
}

/// Does the declared prefix cover this manifest key?
fn scope_covers(declared: &str, manifest_key: &str) -> bool {
    let declared = normalize_scope_entry(declared);
    manifest_key == declared || manifest_key.starts_with(&format!("{declared}/"))
}

/// Is this manifest key a directory *on the way to* a declared path?
fn key_is_ancestor_of_declared(manifest_key: &str, declared: &str) -> bool {
    let declared = normalize_scope_entry(declared);
    declared.starts_with(&format!("{manifest_key}/"))
}

/// Why the gate refused to release the run's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The workspace changed in a way the contract does not allow.
    ProhibitedDelta,
    /// The pre-image or the post-image is incomplete (an entry could not be
    /// read, or the pre-image is missing/corrupt). The gate cannot prove
    /// anything about the workspace, so it fails closed: an unreadable entry is
    /// exactly where a change would hide.
    UnusableImage,
}

impl RejectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::ProhibitedDelta => "prohibited_delta",
            RejectReason::UnusableImage => "unusable_image",
        }
    }
}

/// Why the gate could not reach a terminal decision at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// The worker (or a descendant) is still alive. The lease must NOT be
    /// quarantined or reclaimed while a process can still write into it, and a
    /// post-image taken now would be a mid-write snapshot. The caller must
    /// terminate the process group and re-run the gate.
    DescendantsAlive,
}

impl BlockReason {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockReason::DescendantsAlive => "descendants_alive",
        }
    }
}

/// The gate's terminal call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// No prohibited delta: the run's artifacts may be released.
    Clean { entries_checked: usize },
    /// Prohibited delta (or an image the gate cannot trust): artifacts are
    /// withheld and the lease must be quarantined.
    Rejected {
        reason: RejectReason,
        deltas: Vec<WorkspaceDelta>,
        entries_checked: usize,
    },
    /// The gate could not decide. Artifacts are withheld; the lease is NOT
    /// quarantined (that would race a live writer).
    Blocked { reason: BlockReason, detail: String },
}

/// What THIS RUN established about the capture-time clock — not what is true of
/// `ctime` in general.
///
/// The distinction is the whole point of the type. A receipt that says "ctime
/// cannot be restored by an unprivileged worker" is stating a property of POSIX
/// and passing it off as a finding; a receipt has to say what was measured, so
/// that a run which measured nothing reads differently from a run which measured
/// something. Anything else is the confident-claim shape #1440 exists to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockBarrierEvidence {
    /// This run read a pre-image carrying these per-walk-root barriers and
    /// re-verified that each strictly exceeds every `ctime` recorded for its
    /// root, and that both images used a real ctime witness on a single
    /// filesystem per root.
    Verified(BTreeMap<String, FsTime>),
    /// Nothing was established: the gate did not reach the barrier check
    /// (blocked, unreadable image), or the check failed. The pessimistic value,
    /// and the one every early return uses.
    NotEstablished,
}

impl ClockBarrierEvidence {
    /// `Verified` with an EMPTY map reports `not_established`, deliberately: it
    /// means the image had no entries for any barrier to cover, and a status
    /// field that reads "verified" over nothing is the same overclaim in a
    /// smaller font. Status, prose and JSON all key off the same predicate so
    /// they cannot drift apart.
    pub fn status(&self) -> &'static str {
        match self {
            ClockBarrierEvidence::Verified(roots) if !roots.is_empty() => "verified",
            _ => "not_established",
        }
    }

    /// One sentence naming what was observed. Never a general guarantee.
    pub fn describe(&self) -> String {
        match self {
            ClockBarrierEvidence::Verified(roots) if !roots.is_empty() => {
                let listed = roots
                    .iter()
                    .map(|(label, at)| format!("{label} at {at}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "This run verified a capture-time clock barrier per walk root ({listed}), each \
                     observed by the parent on that root's own filesystem after the walk and \
                     strictly greater than every ctime that root recorded, so an inode change made \
                     after the capture carries a ctime strictly past the recorded one. That is \
                     what this run measured; it is not a claim about ctime beyond these roots, \
                     this filesystem, or a worker holding privileges this gate does not model."
                )
            }
            // `Verified` with no roots means the image had no entries to vouch
            // for. Reported as absent rather than dressed up as a pass.
            _ => "This run did NOT establish a capture-time clock barrier, so a write that landed \
                  inside the capture's own clock tick could carry the recorded stamp and is not \
                  excluded."
                .to_string(),
        }
    }

    /// Receipt shape. One construction, so `status`, `roots` and the prose can
    /// never disagree about the same run.
    pub fn to_json(&self) -> Value {
        let roots = match self {
            ClockBarrierEvidence::Verified(roots) if !roots.is_empty() => Value::Object(
                roots
                    .iter()
                    .map(|(label, at)| (label.clone(), Value::String(at.to_string())))
                    .collect::<serde_json::Map<String, Value>>(),
            ),
            _ => Value::Null,
        };
        json!({
            "status": self.status(),
            "roots": roots,
            "established": self.describe(),
        })
    }
}

/// A gate run: the verdict plus everything a receipt needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    pub env_id: String,
    pub workspace_root: String,
    pub contract_label: &'static str,
    pub declared_scope: Vec<String>,
    pub liveness_probe: String,
    /// Manifest keys whose subtrees were fingerprinted but not content-hashed
    /// (see [`PostflightGate::unhashed_dir_names`]). Empty unless the caller
    /// opted in. Carried on EVERY surface, including a clean one: "I did not
    /// hash it" must never be silently read as "it did not change".
    pub content_unhashed_paths: Vec<String>,
    /// What this run established about the capture-time clock. Carried on EVERY
    /// surface for the same reason `content_unhashed_paths` is: the receipt must
    /// distinguish "I verified a barrier" from "I never got one", and a run that
    /// never got one must not read like a run that did.
    pub clock_barrier: ClockBarrierEvidence,
    pub verdict: GateVerdict,
    pub checked_at: String,
}

impl GateOutcome {
    /// The honest caveat for a run with unhashed subtrees (`None` when the whole
    /// image was hashed).
    ///
    /// It reports **what this run established**, not what is generally true of a
    /// timestamp. The previous wording asserted that ctime "cannot be restored
    /// by an unprivileged worker" and that "the pre-image is sealed with an
    /// observed capture-time clock barrier" as flat guarantees — on every run,
    /// including runs where no barrier was ever verified, from a struct that did
    /// not carry the answer. That is the confident-claim shape this issue exists
    /// to remove, so the barrier sentence now comes from
    /// [`ClockBarrierEvidence`], which is set from the data of the run in hand.
    pub fn unhashed_caveat(&self) -> Option<String> {
        if self.content_unhashed_paths.is_empty() {
            return None;
        }
        Some(format!(
            "content under {} was NOT hashed by this run: {}. For those paths this run compared \
             metadata only (kind/size/mode/inode/nlink/xattr/mtime/ctime); no content digest \
             exists for them. {}",
            if self.content_unhashed_paths.len() == 1 {
                "1 path".to_string()
            } else {
                format!("{} paths", self.content_unhashed_paths.len())
            },
            self.content_unhashed_paths.join(", "),
            self.clock_barrier.describe(),
        ))
    }
    /// May the run's patch / result be handed on? ONLY on a clean verdict.
    pub fn artifacts_released(&self) -> bool {
        matches!(self.verdict, GateVerdict::Clean { .. })
    }

    /// Must the lease be quarantined? Only for a terminal rejection — never
    /// while the gate is blocked on live descendants (enforcement point 5:
    /// quarantine/reclaim happen only after the process tree is gone).
    pub fn lease_quarantine_required(&self) -> bool {
        matches!(self.verdict, GateVerdict::Rejected { .. })
    }

    /// The prohibited deltas, for the receipt.
    pub fn deltas(&self) -> &[WorkspaceDelta] {
        match &self.verdict {
            GateVerdict::Rejected { deltas, .. } => deltas,
            _ => &[],
        }
    }

    pub fn verdict_label(&self) -> &'static str {
        match &self.verdict {
            GateVerdict::Clean { .. } => "clean",
            GateVerdict::Rejected { .. } => "rejected",
            GateVerdict::Blocked { .. } => "blocked",
        }
    }

    /// Gate the run's output on the verdict. This is the "**no patch**" half of
    /// the posture: a rejected (or blocked) run hands its caller an error, not
    /// an artifact — there is no code path that returns both.
    pub fn release<T>(&self, artifact: T) -> Result<T, String> {
        if self.artifacts_released() {
            Ok(artifact)
        } else {
            Err(self
                .failure_message()
                .unwrap_or_else(|| "postflight gate withheld the run artifacts".to_string()))
        }
    }

    /// The loud failure. `None` on a clean verdict.
    pub fn failure_message(&self) -> Option<String> {
        match &self.verdict {
            GateVerdict::Clean { .. } => None,
            GateVerdict::Rejected { reason, deltas, .. } => {
                let mut lines = vec![format!(
                    "{} postflight gate REJECTED lease {} ({}): the workspace at {} changed in \
                     {} way(s) the contract ({}) does not accept. No patch and no result are \
                     emitted; the lease is quarantined. This gate proves {}; it does not prove {}.",
                    naming::POSTURE,
                    self.env_id,
                    reason.as_str(),
                    self.workspace_root,
                    deltas.len(),
                    self.contract_label,
                    naming::PROVES,
                    naming::DOES_NOT_PROVE,
                )];
                for delta in deltas.iter().take(MAX_DELTAS_IN_MESSAGE) {
                    lines.push(format!(
                        "  - {} [{}] {}",
                        delta.path,
                        delta.kind.as_str(),
                        delta.detail
                    ));
                }
                if deltas.len() > MAX_DELTAS_IN_MESSAGE {
                    lines.push(format!(
                        "  - … and {} more (full list in the receipt)",
                        deltas.len() - MAX_DELTAS_IN_MESSAGE
                    ));
                }
                if let Some(caveat) = self.unhashed_caveat() {
                    lines.push(format!("  ! {caveat}"));
                }
                Some(lines.join("\n"))
            }
            GateVerdict::Blocked { reason, detail } => Some(format!(
                "{} postflight gate could not decide for lease {} ({}): {}. Artifacts are \
                 withheld and the lease is left alone — quarantine and reclaim run only after \
                 every descendant process has terminated.",
                naming::POSTURE,
                self.env_id,
                reason.as_str(),
                detail
            )),
        }
    }

    /// The dispatch receipt (item 3): which paths, what kind of delta, what the
    /// gate did about it, and what it does and does not prove.
    pub fn receipt(&self) -> Value {
        let entries_checked = match &self.verdict {
            GateVerdict::Clean { entries_checked } => Some(*entries_checked),
            GateVerdict::Rejected {
                entries_checked, ..
            } => Some(*entries_checked),
            GateVerdict::Blocked { .. } => None,
        };
        let reject_reason = match &self.verdict {
            GateVerdict::Rejected { reason, .. } => Some(reason.as_str()),
            _ => None,
        };
        let block_reason = match &self.verdict {
            GateVerdict::Blocked { reason, .. } => Some(reason.as_str()),
            _ => None,
        };
        let artifacts = if self.artifacts_released() {
            "released"
        } else {
            "withheld"
        };
        let lease_action = if self.lease_quarantine_required() {
            "quarantined"
        } else {
            "none"
        };
        json!({
            "gate": "exec_env_postflight",
            "posture": naming::POSTURE,
            "proves": naming::PROVES,
            "does_not_prove": naming::DOES_NOT_PROVE,
            "env_id": self.env_id,
            "workspace_root": self.workspace_root,
            "contract": self.contract_label,
            "declared_scope": self.declared_scope,
            "liveness_probe": self.liveness_probe,
            "verdict": self.verdict_label(),
            "reject_reason": reject_reason,
            "block_reason": block_reason,
            "entries_checked": entries_checked,
            "content_unhashed_paths": self.content_unhashed_paths,
            "content_unhashed_note": self.unhashed_caveat(),
            // On EVERY receipt, not only the ones with unhashed paths: whether
            // this run resolved its capture window is a property of the run, and
            // a reader must not have to infer it from the absence of a caveat.
            "clock_barrier": self.clock_barrier.to_json(),
            "artifacts": artifacts,
            "lease_action": lease_action,
            "prohibited_deltas": self.deltas(),
            "checked_at": self.checked_at,
        })
    }

    /// The same receipt shaped as a dispatch trajectory event, so a caller can
    /// append it to `trajectory.jsonl` / `progress.jsonl` verbatim.
    pub fn trajectory_event(&self) -> Value {
        let mut event = self.receipt();
        if let Some(obj) = event.as_object_mut() {
            obj.insert(
                "event".to_string(),
                Value::String("exec_env_postflight".to_string()),
            );
            obj.insert(
                "timestamp".to_string(),
                Value::String(self.checked_at.clone()),
            );
        }
        event
    }
}

const MAX_DELTAS_IN_MESSAGE: usize = 20;

/// The gate itself: parent-side pre-image capture, then the postflight compare.
#[derive(Debug, Clone)]
pub struct PostflightGate {
    /// The lease this gate belongs to (`exec_envs.env_id`).
    pub env_id: String,
    /// The lease workspace the worker runs in.
    pub workspace_root: PathBuf,
    /// Where the parent keeps the pre-image. MUST be outside **every**
    /// worker-writable root (enforced) — a pre-image the worker can rewrite
    /// proves nothing.
    pub preimage_path: PathBuf,
    pub contract: WriteContract,
    /// Directory names (e.g. [`manifest::BUILD_ARTIFACT_DIR_NAMES`]) whose
    /// subtrees are walked and fingerprinted but **not content-hashed**.
    ///
    /// Empty by default: hash everything. Set it when the lease workspace can
    /// hold a multi-GB build cache (an in-tree Rust `target/`), where hashing
    /// every artifact would dominate the capture. Detection is *not* dropped for
    /// those paths — size, inode, nlink, mode, xattrs, mtime and ctime are still
    /// fingerprinted, and POSIX offers no call that sets ctime (`utimensat`,
    /// which restores mtime, bumps it) — but the proof there is metadata-only,
    /// and every receipt says so by name (`content_unhashed_paths`), alongside
    /// what that run actually established about the clock
    /// (`clock_barrier`). That metadata-only proof is exactly why the
    /// pre-image is sealed with a capture-time clock barrier: with no content
    /// digest to fall back on, a same-size overwrite has nothing but the
    /// timestamps to testify with.
    pub unhashed_dir_names: Vec<String>,
}

impl PostflightGate {
    /// The strict default: hash every file in the workspace.
    pub fn new(
        env_id: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        preimage_path: impl Into<PathBuf>,
        contract: WriteContract,
    ) -> PostflightGate {
        PostflightGate {
            env_id: env_id.into(),
            workspace_root: workspace_root.into(),
            preimage_path: preimage_path.into(),
            contract,
            unhashed_dir_names: Vec::new(),
        }
    }

    /// Walk build-output dirs but do not hash their contents (see
    /// [`PostflightGate::unhashed_dir_names`]).
    pub fn with_build_artifacts_unhashed(mut self) -> PostflightGate {
        self.unhashed_dir_names = manifest::BUILD_ARTIFACT_DIR_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        self
    }

    /// The worker-writable roots this gate covers: the lease workspace, plus a
    /// linked worktree's external git metadata dir. Both are writable by a
    /// same-UID worker, so both constrain where the pre-image may live.
    fn worker_writable_roots(&self, gitdir: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
        let mut roots = vec![("the lease workspace", self.workspace_root.clone())];
        if let Some(gitdir) = gitdir {
            roots.push((
                "the lease's external git metadata dir (gitdir)",
                gitdir.to_path_buf(),
            ));
        }
        roots
    }

    /// Capture the pre-image. Call this **before the worker is spawned** — a
    /// pre-image taken after the worker starts is worthless.
    ///
    /// This is also the ONLY place the external `gitdir` is resolved: `.git` is a
    /// worker-writable file, so its `gitdir:` target is read here, while the
    /// workspace is still the parent's alone, and then **pinned** into the
    /// pre-image. [`PostflightGate::run`] walks the pinned root and never
    /// re-reads `.git` (see the `manifest` module docs).
    ///
    /// Fails closed on an incomplete image: if the parent cannot read every
    /// entry, it cannot later prove that entry unchanged, so the dispatch must
    /// not start under this contract rather than run and be un-provable at the
    /// end. No pre-image file is written in that case.
    pub fn capture_preimage(&self) -> Result<WorkspaceManifest, String> {
        let gitdir = manifest::resolve_external_git_dir(&self.workspace_root);
        ensure_preimage_outside_worker_writable_roots(
            &self.worker_writable_roots(gitdir.as_deref()),
            &self.preimage_path,
        )?;
        let mut manifest = manifest::capture(&CaptureSpec {
            workspace_root: &self.workspace_root,
            gitdir_root: gitdir.as_deref(),
            unhashed_dir_names: &self.unhashed_dir_names,
        })?;
        if !manifest.is_complete() {
            return Err(format!(
                "pre-image of {} is INCOMPLETE ({} unreadable entr{}), so a later postflight \
                 comparison could not prove those paths unchanged; refusing to capture a \
                 pre-image that cannot convict:\n  - {}",
                self.workspace_root.display(),
                manifest.errors.len(),
                if manifest.errors.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                manifest.errors.join("\n  - ")
            ));
        }
        // A complete image is still not a *provable* one. Two mutation classes —
        // mutate-then-restore, and a same-size overwrite under an unhashed root —
        // have no witness except a timestamp, so the image proves nothing until
        // the parent has OBSERVED the filesystem clock move strictly past every
        // ctime it just recorded. Sealing does that; failing to seal refuses the
        // dispatch rather than starting one whose postflight could not convict
        // (#1440 — an unobserved clock made the gate certify a mutated tree).
        manifest.seal_with_clock_barrier().map_err(|e| {
            format!(
                "pre-image of {} cannot be sealed against the filesystem clock, so a later \
                 postflight comparison could not distinguish an untouched entry from one written \
                 inside the capture's own clock tick; refusing to capture a pre-image that cannot \
                 convict: {e}",
                self.workspace_root.display()
            )
        })?;
        let bytes = serde_json::to_vec(&manifest)
            .map_err(|e| format!("serialize workspace pre-image: {e}"))?;
        crate::utils::write_owner_only_file_atomic(&self.preimage_path, &bytes)
            .map_err(|e| format!("write workspace pre-image: {e}"))?;
        Ok(manifest)
    }

    /// Run the gate after the worker has terminated.
    ///
    /// Order is load-bearing:
    /// 1. **descendants first** — a live process means no decision, no
    ///    quarantine, no reclaim (enforcement point 5);
    /// 2. then load the parent-held pre-image (which carries the **pinned** walk
    ///    roots);
    /// 3. then re-scan **those roots** and compare;
    /// 3b. then check the pre-image's **capture-time clock barrier** — an image
    ///    whose timestamps cannot out-resolve the run window cannot prove a
    ///    match means "unchanged";
    /// 4. then apply the contract.
    ///
    /// Step 3 never re-derives a walk root from the workspace: the worker has had
    /// write access to `.git`, so re-reading its `gitdir:` line here would let the
    /// worker pick what the parent hashes (a decoy root, or `/` as a DoS). The
    /// rewrite of `.git` itself is still caught — it is a content delta on
    /// `workspace/.git` like any other file.
    pub fn run(&self, liveness: &dyn DescendantLiveness) -> Result<GateOutcome, String> {
        let checked_at = chrono::Utc::now().to_rfc3339();
        // `barrier` is an explicit parameter rather than a field defaulted
        // somewhere convenient: every early return has to name what it
        // established, and the only value it can name before step 3b is
        // `NotEstablished`.
        let base = |verdict: GateVerdict, unhashed: Vec<String>, barrier: ClockBarrierEvidence| {
            GateOutcome {
                env_id: self.env_id.clone(),
                workspace_root: self.workspace_root.to_string_lossy().to_string(),
                contract_label: self.contract.label(),
                declared_scope: self.contract.declared_paths(),
                liveness_probe: liveness.describe(),
                content_unhashed_paths: unhashed,
                clock_barrier: barrier,
                verdict,
                checked_at: checked_at.clone(),
            }
        };

        // (1) Never scan or tear down a workspace a live process can still write.
        if liveness.any_alive()? {
            return Ok(base(
                GateVerdict::Blocked {
                    reason: BlockReason::DescendantsAlive,
                    detail: format!(
                        "{} still has at least one live process; terminate the worker tree, then \
                         re-run the gate",
                        liveness.describe()
                    ),
                },
                Vec::new(),
                ClockBarrierEvidence::NotEstablished,
            ));
        }

        // (2) The pre-image is parent-held; a missing or corrupt one is not a
        // pass, it is an unusable image.
        let pre = match self.load_preimage() {
            Ok(pre) => pre,
            Err(e) => {
                return Ok(base(
                    GateVerdict::Rejected {
                        reason: RejectReason::UnusableImage,
                        deltas: vec![WorkspaceDelta {
                            path: self.preimage_path.to_string_lossy().to_string(),
                            kind: DeltaKind::Unreadable,
                            detail: format!("pre-image unusable: {e}"),
                            facets: Vec::new(),
                            entry_kind: None,
                        }],
                        entries_checked: 0,
                    },
                    Vec::new(),
                    ClockBarrierEvidence::NotEstablished,
                ))
            }
        };

        // (3) Re-scan — against the roots the PARENT pinned before the spawn.
        // A pinned gitdir the worker deleted or moved fails closed as an
        // unreadable (and therefore unprovable) root, not as a clean run.
        let pinned_gitdir = pre.gitdir_root.as_ref().map(PathBuf::from);
        let post = match manifest::capture(&CaptureSpec {
            workspace_root: &self.workspace_root,
            gitdir_root: pinned_gitdir.as_deref(),
            unhashed_dir_names: &self.unhashed_dir_names,
        }) {
            Ok(post) => post,
            Err(e) => {
                return Ok(base(
                    GateVerdict::Rejected {
                        reason: RejectReason::UnusableImage,
                        deltas: vec![WorkspaceDelta {
                            path: self.workspace_root.to_string_lossy().to_string(),
                            kind: DeltaKind::Unreadable,
                            detail: format!("post-image could not be captured: {e}"),
                            facets: Vec::new(),
                            entry_kind: None,
                        }],
                        entries_checked: 0,
                    },
                    Vec::new(),
                    ClockBarrierEvidence::NotEstablished,
                ))
            }
        };

        let entries_checked = pre.len().max(post.len());
        let unhashed = merge_unhashed(&pre, &post);

        // An incomplete image on either side cannot prove a clean run. (The
        // pre-image side is belt-and-braces: `capture_preimage` already refuses
        // to write an incomplete one.)
        if !pre.is_complete() || !post.is_complete() {
            let workspace = self.workspace_root.to_string_lossy().to_string();
            let deltas = pre
                .errors
                .iter()
                .map(|e| format!("pre-image capture: {e}"))
                .chain(
                    post.errors
                        .iter()
                        .map(|e| format!("post-image capture: {e}")),
                )
                .map(|detail| WorkspaceDelta {
                    path: workspace.clone(),
                    kind: DeltaKind::Unreadable,
                    detail: format!("unreadable, so it cannot be proven unchanged: {detail}"),
                    facets: Vec::new(),
                    entry_kind: None,
                })
                .collect();
            return Ok(base(
                GateVerdict::Rejected {
                    reason: RejectReason::UnusableImage,
                    deltas,
                    entries_checked,
                },
                unhashed,
                ClockBarrierEvidence::NotEstablished,
            ));
        }

        // (3b) A complete pre-image whose timestamps cannot out-resolve the run
        // window is not a pass either. For the classes whose ONLY witness is a
        // timestamp, "every field matched" is indistinguishable from "the clock
        // never ticked" unless the capture recorded an observed barrier past
        // every ctime. Re-checked here from the data, not trusted: an image from
        // an older binary carries no barrier and must fail closed, not sail
        // through (#1440).
        //
        // The POST-image is checked too, and not as a formality. A comparison
        // has two sides: the barrier bounds when the pre-image's ctimes were
        // taken, but it is the post-image's ctime that has to be *seen* moving
        // past them. A post-image captured with no ctime witness, or spanning a
        // second filesystem the barrier never measured, makes the comparison
        // unsound however well-sealed the pre-image is — and a mount appearing
        // under the walk root during the run is exactly a thing that happens
        // between capture and gate.
        let barrier_check = pre
            .verify_clock_barriers()
            .map_err(|why| (self.preimage_path.to_string_lossy().to_string(), why))
            .and_then(|()| {
                post.verify_timestamp_preconditions()
                    .map_err(|why| (self.workspace_root.to_string_lossy().to_string(), why))
            });
        if let Err((path, why)) = barrier_check {
            return Ok(base(
                GateVerdict::Rejected {
                    reason: RejectReason::UnusableImage,
                    deltas: vec![WorkspaceDelta {
                        path,
                        kind: DeltaKind::Unreadable,
                        detail: format!(
                            "this run has no usable capture-time clock barrier over the compared \
                             images, so an entry whose fields all match cannot be proven \
                             unchanged: {why}"
                        ),
                        facets: Vec::new(),
                        entry_kind: None,
                    }],
                    entries_checked,
                },
                unhashed,
                ClockBarrierEvidence::NotEstablished,
            ));
        }
        // Past this line — and only past it — the run has something to report.
        let barrier = ClockBarrierEvidence::Verified(pre.clock_barriers.clone());

        // (4) Compare, then apply the contract.
        let prohibited: Vec<WorkspaceDelta> = manifest::diff(&pre, &post)
            .into_iter()
            .filter(|delta| !self.contract.permits(delta))
            .collect();

        if prohibited.is_empty() {
            Ok(base(
                GateVerdict::Clean { entries_checked },
                unhashed,
                barrier,
            ))
        } else {
            Ok(base(
                GateVerdict::Rejected {
                    reason: RejectReason::ProhibitedDelta,
                    deltas: prohibited,
                    entries_checked,
                },
                unhashed,
                barrier,
            ))
        }
    }

    fn load_preimage(&self) -> Result<WorkspaceManifest, String> {
        let bytes = std::fs::read(&self.preimage_path).map_err(|e| {
            format!(
                "read pre-image {}: {e}",
                self.preimage_path.to_string_lossy()
            )
        })?;
        serde_json::from_slice(&bytes).map_err(|e| format!("parse pre-image: {e}"))
    }
}

/// The union of the two images' unhashed roots, deduped. Taken from BOTH sides:
/// a `target/` the worker created mid-run appears only in the post-image, and the
/// receipt must still disclose that its contents were not hashed.
fn merge_unhashed(pre: &WorkspaceManifest, post: &WorkspaceManifest) -> Vec<String> {
    let mut all: Vec<String> = pre
        .unhashed_roots
        .iter()
        .chain(post.unhashed_roots.iter())
        .cloned()
        .collect();
    all.sort();
    all.dedup();
    all
}

/// The pre-image must live where the worker cannot reach it — and this module
/// declares **two** worker-writable roots, not one: the lease workspace *and* a
/// linked worktree's external git metadata dir (`gitdir`). A pre-image inside
/// either could be rewritten by the very worker it is meant to convict (the gate
/// would then compare the worker's own story against itself), so this is a hard
/// fail, not a warning.
///
/// `roots` is `(human label, path)`; every root the caller can name as
/// worker-writable must be in it.
pub fn ensure_preimage_outside_worker_writable_roots(
    roots: &[(&str, PathBuf)],
    preimage_path: &Path,
) -> Result<(), String> {
    let preimage = canonical_or_literal(preimage_path);
    for (label, root) in roots {
        let root = canonical_or_literal(root);
        if preimage.starts_with(&root) {
            return Err(format!(
                "pre-image {} is inside {label} {} — the worker could rewrite it; the pre-image \
                 must be held by the parent, outside EVERY root the worker can write",
                preimage.display(),
                root.display()
            ));
        }
    }
    Ok(())
}

/// Canonicalize when possible (resolves `..`, symlinked temp dirs such as
/// macOS `/var` → `/private/var`), otherwise fall back to the literal path — a
/// not-yet-created pre-image file has no canonical form.
fn canonical_or_literal(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match path.parent() {
        Some(parent) => match parent.canonicalize() {
            Ok(canonical_parent) => match path.file_name() {
                Some(name) => canonical_parent.join(name),
                None => canonical_parent,
            },
            Err(_) => path.to_path_buf(),
        },
        None => path.to_path_buf(),
    }
}

/// Where a rejected lease's evidence goes.
///
/// Quarantine is deliberately NOT reclaim: the bytes are preserved for
/// forensics. Reclaiming (deleting) a workspace that just failed the gate would
/// destroy the only evidence of what the worker did.
pub trait QuarantineSink {
    fn quarantine(&self, outcome: &GateOutcome) -> Result<(), String>;
}

/// Parent-side forensic sink: writes the receipt to an owner-only file outside
/// the workspace and logs the failure loudly.
#[derive(Debug, Clone)]
pub struct FileQuarantineSink {
    pub dir: PathBuf,
}

impl QuarantineSink for FileQuarantineSink {
    fn quarantine(&self, outcome: &GateOutcome) -> Result<(), String> {
        let file = self.dir.join(format!(
            "{}-{}.json",
            crate::utils::sanitize_safe_path_name(&outcome.env_id),
            crate::utils::sanitize_safe_path_name(&outcome.checked_at)
        ));
        let bytes = serde_json::to_vec_pretty(&outcome.receipt())
            .map_err(|e| format!("serialize quarantine receipt: {e}"))?;
        crate::utils::write_owner_only_file_atomic(&file, &bytes)
            .map_err(|e| format!("write quarantine receipt: {e}"))?;
        tracing::error!(
            env_id = %outcome.env_id,
            workspace = %outcome.workspace_root,
            deltas = outcome.deltas().len(),
            receipt = %file.display(),
            "{}",
            rejection_log_message(outcome)
        );
        Ok(())
    }
}

/// The log line for a rejected run. Kept as a function (not an inline
/// `tracing::error!` literal) so the naming-discipline test can assert what
/// this gate actually says in the log, not just in its receipts.
pub fn rejection_log_message(outcome: &GateOutcome) -> String {
    outcome.failure_message().unwrap_or_else(|| {
        let mut line = format!(
            "{} postflight gate: lease {} passed ({})",
            naming::POSTURE,
            outcome.env_id,
            naming::PROVES
        );
        // A clean verdict must carry its own asterisk: if a subtree was not
        // hashed, say so on the pass line, not only in the receipt.
        if let Some(caveat) = outcome.unhashed_caveat() {
            line.push_str(&format!(" — {caveat}"));
        }
        line
    })
}

/// Apply the gate's decision to the lease: quarantine on rejection, nothing
/// otherwise. Returns whether the lease was quarantined.
///
/// The S2a resource ledger (`exec_env_resources.state = 'quarantined'`) is a
/// second [`QuarantineSink`] over the same call: state transitions stay
/// single-writer inside the ledger's own reclaim function, and this gate never
/// writes lease state directly.
pub fn apply_verdict(outcome: &GateOutcome, sink: &dyn QuarantineSink) -> Result<bool, String> {
    if !outcome.lease_quarantine_required() {
        return Ok(false);
    }
    sink.quarantine(outcome)?;
    Ok(true)
}
