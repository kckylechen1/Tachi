//! `detect-and-reject` postflight gate (#894 S2e) — ordered enforcement points
//! 4 (parent-owned postflight manifest/diff gate) and 5 (quarantine/reclaim
//! only after descendants terminate).
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

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub use liveness::{DescendantLiveness, ProcessGroupLiveness};
pub use manifest::{DeltaKind, WorkspaceDelta, WorkspaceManifest};

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
                // A directory on the path to a declared write records that
                // write in its own mtime/ctime and in nothing else. Forgiving
                // exactly that — and only for an ancestor of a declared path —
                // is what keeps a declared scope usable. A chmod, an xattr, an
                // inode swap or a content change on the same directory still
                // rejects.
                delta.is_time_only_metadata()
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

/// A gate run: the verdict plus everything a receipt needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    pub env_id: String,
    pub workspace_root: String,
    pub contract_label: &'static str,
    pub declared_scope: Vec<String>,
    pub liveness_probe: String,
    pub verdict: GateVerdict,
    pub checked_at: String,
}

impl GateOutcome {
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
    /// Where the parent keeps the pre-image. MUST be outside `workspace_root`
    /// (enforced) — a pre-image the worker can rewrite proves nothing.
    pub preimage_path: PathBuf,
    pub contract: WriteContract,
}

impl PostflightGate {
    /// Capture the pre-image. Call this **before the worker is spawned** — a
    /// pre-image taken after the worker starts is worthless.
    ///
    /// Fails closed on an incomplete image: if the parent cannot read every
    /// entry, it cannot later prove that entry unchanged, so the dispatch must
    /// not start under this contract rather than run and be un-provable at the
    /// end. No pre-image file is written in that case.
    pub fn capture_preimage(&self) -> Result<WorkspaceManifest, String> {
        ensure_preimage_outside_workspace(&self.workspace_root, &self.preimage_path)?;
        let manifest = manifest::capture(&self.workspace_root)?;
        if !manifest.is_complete() {
            return Err(format!(
                "pre-image of {} is INCOMPLETE ({} unreadable entr{}), so a later postflight \
                 comparison could not prove those paths unchanged; refusing to capture a \
                 pre-image that cannot convict:\n  - {}",
                self.workspace_root.display(),
                manifest.errors.len(),
                if manifest.errors.len() == 1 { "y" } else { "ies" },
                manifest.errors.join("\n  - ")
            ));
        }
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
    /// 2. then load the parent-held pre-image;
    /// 3. then re-scan and compare;
    /// 4. then apply the contract.
    pub fn run(&self, liveness: &dyn DescendantLiveness) -> Result<GateOutcome, String> {
        let checked_at = chrono::Utc::now().to_rfc3339();
        let base = |verdict: GateVerdict| GateOutcome {
            env_id: self.env_id.clone(),
            workspace_root: self.workspace_root.to_string_lossy().to_string(),
            contract_label: self.contract.label(),
            declared_scope: self.contract.declared_paths(),
            liveness_probe: liveness.describe(),
            verdict,
            checked_at: checked_at.clone(),
        };

        // (1) Never scan or tear down a workspace a live process can still write.
        if liveness.any_alive()? {
            return Ok(base(GateVerdict::Blocked {
                reason: BlockReason::DescendantsAlive,
                detail: format!(
                    "{} still has at least one live process; terminate the worker tree, then \
                     re-run the gate",
                    liveness.describe()
                ),
            }));
        }

        // (2) The pre-image is parent-held; a missing or corrupt one is not a
        // pass, it is an unusable image.
        let pre = match self.load_preimage() {
            Ok(pre) => pre,
            Err(e) => {
                return Ok(base(GateVerdict::Rejected {
                    reason: RejectReason::UnusableImage,
                    deltas: vec![WorkspaceDelta {
                        path: self.preimage_path.to_string_lossy().to_string(),
                        kind: DeltaKind::Unreadable,
                        detail: format!("pre-image unusable: {e}"),
                        facets: Vec::new(),
                    }],
                    entries_checked: 0,
                }))
            }
        };

        // (3) Re-scan.
        let post = match manifest::capture(&self.workspace_root) {
            Ok(post) => post,
            Err(e) => {
                return Ok(base(GateVerdict::Rejected {
                    reason: RejectReason::UnusableImage,
                    deltas: vec![WorkspaceDelta {
                        path: self.workspace_root.to_string_lossy().to_string(),
                        kind: DeltaKind::Unreadable,
                        detail: format!("post-image could not be captured: {e}"),
                        facets: Vec::new(),
                    }],
                    entries_checked: 0,
                }))
            }
        };

        let entries_checked = pre.len().max(post.len());

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
                })
                .collect();
            return Ok(base(GateVerdict::Rejected {
                reason: RejectReason::UnusableImage,
                deltas,
                entries_checked,
            }));
        }

        // (4) Compare, then apply the contract.
        let prohibited: Vec<WorkspaceDelta> = manifest::diff(&pre, &post)
            .into_iter()
            .filter(|delta| !self.contract.permits(delta))
            .collect();

        if prohibited.is_empty() {
            Ok(base(GateVerdict::Clean { entries_checked }))
        } else {
            Ok(base(GateVerdict::Rejected {
                reason: RejectReason::ProhibitedDelta,
                deltas: prohibited,
                entries_checked,
            }))
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

/// The pre-image must live where the worker cannot reach it. A pre-image inside
/// the workspace could be rewritten by the very worker it is meant to convict,
/// so this is a hard fail, not a warning.
pub fn ensure_preimage_outside_workspace(
    workspace_root: &Path,
    preimage_path: &Path,
) -> Result<(), String> {
    let workspace = canonical_or_literal(workspace_root);
    let preimage = canonical_or_literal(preimage_path);
    if preimage.starts_with(&workspace) {
        return Err(format!(
            "pre-image {} is inside the lease workspace {} — the worker could rewrite it; the \
             pre-image must be held by the parent, outside anything the worker can write",
            preimage.display(),
            workspace.display()
        ));
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
        format!(
            "{} postflight gate: lease {} passed ({})",
            naming::POSTURE,
            outcome.env_id,
            naming::PROVES
        )
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
