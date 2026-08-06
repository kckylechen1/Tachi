//! Target-dir generations and the poisoning defense (#894 S2c item 3).
//!
//! ## The bug this exists to prevent
//!
//! A single writer only kills *concurrent* poisoning. It does not kill
//! **serial** poisoning: one executor seat, running one build at a time,
//! reusing one target dir across tickets whose source trees have **diverged**,
//! still corrupts that target. Observed twice on this machine on 2026-07-13: a
//! symbol you can `grep` in the source, reported by rustc as `not found`.
//!
//! So the seat holds:
//! - one fixed clean checkout (it never "just checks out a branch" somewhere
//!   dirty),
//! - one **resident** target for the main lineage,
//! - at most one **scratch** target (TTL) for forks.
//!
//! And before `cargo` starts, [`plan_target`] must confirm the chosen target's
//! *generation* (the source identity that last drove it) is lineage-compatible
//! with the ticket's source identity. Incompatible → use/clear the scratch
//! target. Never a bare reuse.
//!
//! "Lineage-compatible" = same repo AND one of the two commits is an ancestor of
//! the other (fast-forward in either direction — cargo handles moving along a
//! line just fine; what it does not survive is two *divergent* trees writing the
//! same fingerprints). Divergence is the fork case.

use memcore::MemoryStore;
use serde::{Deserialize, Serialize};

use super::ticket::SourceIdentity;

/// `hard_state` namespace for target generations; key = the target dir path.
pub const GENERATION_NS: &str = "build_target_generation";

/// What last drove a target dir. This is the "generation" the compatibility
/// check runs against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetGeneration {
    pub repo_root: String,
    pub head_sha: String,
    pub ticket_id: String,
    pub stamped_at: String,
}

/// Which of the seat's two targets a plan picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSlotKind {
    /// The seat's long-lived main-lineage target.
    Resident,
    /// The seat's one TTL fork target.
    Scratch,
}

impl TargetSlotKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetSlotKind::Resident => "resident",
            TargetSlotKind::Scratch => "scratch",
        }
    }
}

/// The observed state of one of the seat's target dirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSlotState {
    pub path: String,
    /// `None` = never driven by any build (virgin dir): compatible with
    /// anything.
    pub generation: Option<TargetGeneration>,
    /// The resource ledger says this dir is quarantined — an interrupted cargo
    /// was writing into it and its contents are not trustworthy (#894 S2c item
    /// 4).
    pub quarantined: bool,
}

/// The decision: which target dir this ticket may use, and whether it must be
/// wiped first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPlan {
    pub slot: TargetSlotKind,
    pub path: String,
    /// The target dir must be wiped (and, if quarantined, released) BEFORE
    /// cargo starts. Never `false` for a quarantined or foreign-generation dir.
    pub clear_first: bool,
    pub reason: String,
}

/// Answers "are these two commits on one line of history?" for a repo.
///
/// Injected so the decision logic is testable without a git repo — and because
/// the production answer is a subprocess (`git merge-base --is-ancestor`) that
/// has no business being inside the planner.
pub trait LineageOracle {
    /// True iff `a` and `b` are the same commit, or one is an ancestor of the
    /// other. False = the trees have DIVERGED (the fork case).
    fn same_lineage(&self, repo_root: &str, a: &str, b: &str) -> Result<bool, String>;
}

/// Choose the target dir for `source`, given the seat's two slots.
///
/// The frozen invariant (#894 S2c item 3, the poisoning defense): **a target
/// whose generation has diverged from the ticket's source is never returned for
/// bare reuse.** Either the plan picks the other slot, or it sets
/// `clear_first`. A quarantined slot is likewise never returned without
/// `clear_first`. Both are re-asserted defensively at the bottom of this
/// function: if the logic above ever drifts, this errors out rather than
/// handing back a poisoned target.
pub fn plan_target(
    source: &SourceIdentity,
    resident: &TargetSlotState,
    scratch: &TargetSlotState,
    lineage: &dyn LineageOracle,
) -> Result<TargetPlan, String> {
    let resident_compatible = generation_compatible(source, resident, lineage)?;

    let plan = if resident_compatible {
        TargetPlan {
            slot: TargetSlotKind::Resident,
            path: resident.path.clone(),
            clear_first: resident.quarantined,
            reason: if resident.quarantined {
                "resident target is on this ticket's lineage but is QUARANTINED (an interrupted \
                 cargo was writing into it): it must be cleared before reuse"
                    .to_string()
            } else {
                match &resident.generation {
                    None => "resident target has never been driven by a build (virgin)".to_string(),
                    Some(gen) => format!(
                        "resident target's generation ({}) is on the same lineage as this \
                         ticket's source ({})",
                        short(&gen.head_sha),
                        short(&source.head_sha)
                    ),
                }
            },
        }
    } else {
        // The ticket's source has DIVERGED from what last drove the resident
        // target. Reusing it is exactly the phantom-compile-error bug. Fall to
        // the scratch target, wiping it unless it happens to already be on this
        // ticket's lineage.
        let scratch_compatible = generation_compatible(source, scratch, lineage)?;
        let clear_first = scratch.quarantined || !scratch_compatible;
        let resident_gen = resident
            .generation
            .as_ref()
            .map(|g| short(&g.head_sha))
            .unwrap_or_else(|| "none".to_string());
        TargetPlan {
            slot: TargetSlotKind::Scratch,
            path: scratch.path.clone(),
            clear_first,
            reason: format!(
                "ticket source ({}) has diverged from the resident target's generation ({}): \
                 using the fork scratch target{}",
                short(&source.head_sha),
                resident_gen,
                if clear_first {
                    ", cleared first"
                } else {
                    " (already on this lineage)"
                }
            ),
        }
    };

    // Defensive re-assertion of the two invariants this function exists for. A
    // bug above must fail loudly, not hand back a poisoned target dir.
    let chosen = match plan.slot {
        TargetSlotKind::Resident => resident,
        TargetSlotKind::Scratch => scratch,
    };
    if chosen.quarantined && !plan.clear_first {
        return Err(format!(
            "BUG (#894 S2c): planned a bare reuse of QUARANTINED target '{}' — refusing",
            chosen.path
        ));
    }
    if !plan.clear_first && !generation_compatible(source, chosen, lineage)? {
        return Err(format!(
            "BUG (#894 S2c): planned a bare reuse of target '{}' whose generation has diverged \
             from the ticket source — refusing (this is the phantom-compile-error path)",
            chosen.path
        ));
    }
    Ok(plan)
}

/// Is this target dir's generation on the ticket's lineage? A virgin dir (no
/// generation) is compatible with everything; a dir last driven by a *different
/// repo* never is.
fn generation_compatible(
    source: &SourceIdentity,
    slot: &TargetSlotState,
    lineage: &dyn LineageOracle,
) -> Result<bool, String> {
    let Some(generation) = &slot.generation else {
        return Ok(true);
    };
    if generation.repo_root != source.repo_root {
        return Ok(false);
    }
    if generation.head_sha == source.head_sha {
        return Ok(true);
    }
    lineage.same_lineage(&source.repo_root, &generation.head_sha, &source.head_sha)
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// Read the generation stamp for a target dir.
pub fn read_generation(
    store: &MemoryStore,
    target_path: &str,
) -> Result<Option<TargetGeneration>, String> {
    let row = store
        .get_state_kv(GENERATION_NS, target_path)
        .map_err(|e| format!("read target generation for {target_path}: {e}"))?;
    match row {
        None => Ok(None),
        Some((json, _version)) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("decode target generation for {target_path}: {e}")),
    }
}

/// Stamp the generation a build just drove into a target dir.
///
/// Called for BOTH successful and failed builds: a failed `cargo` still wrote
/// artifacts and fingerprints into that dir, so it still defines the dir's
/// generation. It is deliberately NOT called for an interrupted build — that
/// target is quarantined instead, because we do not know what state it reached.
pub fn stamp_generation(
    store: &MemoryStore,
    target_path: &str,
    generation: &TargetGeneration,
) -> Result<(), String> {
    let value =
        serde_json::to_string(generation).map_err(|e| format!("serialize generation: {e}"))?;
    store
        .set_state(GENERATION_NS, target_path, &value)
        .map_err(|e| format!("stamp target generation for {target_path}: {e}"))?;
    Ok(())
}

/// Forget a target dir's generation (it was wiped — it is virgin again).
pub fn clear_generation(store: &MemoryStore, target_path: &str) -> Result<(), String> {
    store
        .delete_state(GENERATION_NS, target_path)
        .map_err(|e| format!("clear target generation for {target_path}: {e}"))?;
    Ok(())
}

/// Production [`LineageOracle`]: `git merge-base --is-ancestor`, both ways.
pub struct GitLineage;

impl LineageOracle for GitLineage {
    fn same_lineage(&self, repo_root: &str, a: &str, b: &str) -> Result<bool, String> {
        // Re-validate the shas here even though `BuildTicket::new` already did:
        // this function shells out to git with them, and a generation stamp read
        // back from the DB has not been through the ticket constructor.
        for sha in [a, b] {
            if !(7..=64).contains(&sha.len()) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "refusing to pass non-object-id '{sha}' to git (#894 S2c)"
                ));
            }
        }
        if a == b {
            return Ok(true);
        }
        Ok(is_ancestor(repo_root, a, b)? || is_ancestor(repo_root, b, a)?)
    }
}

fn is_ancestor(repo_root: &str, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(ancestor)
        .arg(descendant)
        .status()
        .map_err(|e| format!("git merge-base --is-ancestor: {e}"))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        // Anything else (128 = unknown revision, etc.) is NOT "they diverged" —
        // it is "we could not tell". Fail closed: an unknown answer must not be
        // reported as compatible.
        other => Err(format!(
            "git merge-base --is-ancestor {ancestor} {descendant} in {repo_root} exited with \
             {other:?}: cannot establish lineage, refusing to guess (#894 S2c)"
        )),
    }
}
