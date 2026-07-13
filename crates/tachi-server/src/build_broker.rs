//! The build broker (#894 S2c item 3): one active `cargo` per machine, and it
//! never runs against a target dir that has diverged from what it is building.
//!
//! ## Why a broker at all
//!
//! Before this, every managed worktree got a `.cargo/config.toml` pointing at
//! ONE machine-shared `CARGO_TARGET_DIR` (#484, to stop each tree growing its
//! own multi-GB `target/`). N agents, N diverged worktrees, one target dir. The
//! result on 2026-07-13 was phantom compile errors — a symbol you can `grep` in
//! the source, reported by rustc as `not found` — twice in one night.
//!
//! Serializing the compiles is only half the fix. A single writer kills
//! *concurrent* poisoning; it does nothing about **serial** poisoning, where the
//! one seat reuses one target across tickets from *forked* trees. So the seat is
//! defined as:
//!
//! - **one fixed clean checkout** — it never checks out over dirty work;
//! - **one resident target** for the main lineage;
//! - **at most one TTL scratch target** for forks;
//! - **a generation check before every cargo**: the target's last-driver must be
//!   lineage-compatible with the ticket's source, or the target is swapped/wiped.
//!   Never a bare reuse (`target::plan_target`).
//!
//! ## The pieces
//!
//! - [`ticket`] — the immutable request (write-once in `hard_state`).
//! - [`slot`] — the machine-unique executor slot (`INSERT ... ON CONFLICT DO
//!   NOTHING`, so exactly one winner among any number of concurrent callers).
//! - [`target`] — generations + the compatibility plan (the poisoning defense).
//! - [`runner`] — git checkout / subprocess / wipe, behind a trait.
//!
//! ## Interruption
//!
//! A `cargo` killed by a signal died with its hands in the target dir. That
//! target is [`memcore::quarantine_resource`]d: nothing can bind it, nothing can
//! reclaim it, and the next build cannot pick it up — the only way out is
//! `release_quarantine`, which forces a wipe/verify first. A daemon that dies
//! mid-build leaves the slot held; recovery is explicit
//! ([`abandon_stale_slot`]), never a TTL steal (stealing a slot whose cargo is
//! still alive would put two compilers in one target dir — the exact bug, with a
//! timer as the trigger).

pub(crate) mod runner;
pub(crate) mod slot;
pub(crate) mod target;
pub(crate) mod ticket;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use memcore::{MemoryStore, ResourceKind, ResourceState};
use serde::{Deserialize, Serialize};

use runner::{BuildOutcome, BuildRunner};
use slot::SlotOutcome;
use target::{LineageOracle, TargetGeneration, TargetPlan, TargetSlotState};
use ticket::BuildTicket;

/// `hard_state` namespace for build receipts; key = ticket id.
pub(crate) const RECEIPT_NS: &str = "build_receipt";

/// The machine's one build executor seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutorSeat {
    /// The seat's single fixed checkout. Every ticket is built here, on a
    /// detached HEAD at the ticket's source sha.
    pub checkout: PathBuf,
    /// The resident main-lineage target dir.
    pub resident_target: PathBuf,
    /// The one TTL scratch target dir, for forked sources.
    pub scratch_target: PathBuf,
}

/// The result, bound to the ticket that asked for it. Write-once, like the
/// ticket: a receipt that could be rewritten is not evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BuildReceipt {
    pub ticket_id: String,
    pub outcome: BuildOutcome,
    pub exit_code: Option<i32>,
    /// Which target dir the build actually ran against, and which seat slot it
    /// was.
    pub target_path: String,
    pub target_slot: String,
    /// Whether the target had to be wiped before this build (a fork, or a
    /// quarantined dir being recovered).
    pub cleared_target: bool,
    /// Set when this build's interruption quarantined its target dir.
    pub quarantined_target: Option<String>,
    /// The lease this result is attributed to.
    pub env_id: Option<String>,
    pub source_head_sha: String,
    pub started_at: String,
    pub finished_at: String,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// Read a ticket's receipt, if the build has already run.
pub(crate) fn load_receipt(
    store: &MemoryStore,
    ticket_id: &str,
) -> Result<Option<BuildReceipt>, String> {
    let row = store
        .get_state_kv(RECEIPT_NS, ticket_id)
        .map_err(|e| format!("load receipt {ticket_id}: {e}"))?;
    match row {
        None => Ok(None),
        Some((json, _version)) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("decode receipt {ticket_id}: {e}")),
    }
}

fn write_receipt(store: &MemoryStore, receipt: &BuildReceipt) -> Result<(), String> {
    let value = serde_json::to_string(receipt).map_err(|e| format!("serialize receipt: {e}"))?;
    store
        .insert_state_if_absent(RECEIPT_NS, &receipt.ticket_id, &value)
        .map_err(|e| format!("write receipt {}: {e}", receipt.ticket_id))?;
    Ok(())
}

/// The queue: submitted tickets with no receipt yet, oldest first.
pub(crate) fn pending_tickets(store: &MemoryStore) -> Result<Vec<BuildTicket>, String> {
    let mut pending = Vec::new();
    for ticket in ticket::list_tickets(store)? {
        if load_receipt(store, &ticket.ticket_id)?.is_none() {
            pending.push(ticket);
        }
    }
    Ok(pending)
}

/// Take the oldest pending ticket and run it, if the executor slot is free.
/// `Ok(None)` = nothing to do (empty queue) or the slot is busy — both are
/// normal, neither is an error.
pub(crate) fn run_next(
    store: &mut MemoryStore,
    seat: &ExecutorSeat,
    runner: &dyn BuildRunner,
    lineage: &dyn LineageOracle,
) -> Result<Option<BuildReceipt>, String> {
    let Some(ticket) = pending_tickets(store)?.into_iter().next() else {
        return Ok(None);
    };
    match execute_ticket(store, seat, &ticket, runner, lineage) {
        Ok(receipt) => Ok(Some(receipt)),
        Err(err) if err.starts_with(SLOT_BUSY) => {
            tracing::debug!(error = %err, "build broker: executor slot busy, deferring");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

const SLOT_BUSY: &str = "executor slot busy";

/// Run one ticket through the seat.
///
/// Order is load-bearing:
/// 1. take the machine's single executor slot (or bail — never a second cargo);
/// 2. plan the target dir against its generation (the poisoning defense);
/// 3. clear/release-quarantine the target if the plan says so;
/// 4. put the seat's fixed checkout on the ticket's source;
/// 5. run;
/// 6. interrupted → quarantine the target and DO NOT stamp a generation
///    (we do not know what state it reached); success/failure → stamp it (a
///    failed cargo still wrote artifacts into that dir);
/// 7. write the receipt, release the slot.
///
/// The slot is released on every exit path after acquisition, including errors.
pub(crate) fn execute_ticket(
    store: &mut MemoryStore,
    seat: &ExecutorSeat,
    ticket: &BuildTicket,
    runner: &dyn BuildRunner,
    lineage: &dyn LineageOracle,
) -> Result<BuildReceipt, String> {
    if let Some(existing) = load_receipt(store, &ticket.ticket_id)? {
        // Already built. Receipts are write-once, so re-running would produce a
        // result nobody can record — and would burn the seat on a done ticket.
        return Ok(existing);
    }

    match slot::acquire_slot(store, &ticket.ticket_id)? {
        SlotOutcome::Acquired => {}
        SlotOutcome::Busy { holder } => {
            return Err(format!(
                "{SLOT_BUSY}: held by ticket '{}' (pid {}) since {}. This machine runs exactly \
                 one cargo at a time (#894 S2c)",
                holder.ticket_id, holder.pid, holder.acquired_at
            ));
        }
    }

    let result = execute_holding_slot(store, seat, ticket, runner, lineage);

    // Always give the slot back — an error path that keeps the slot would wedge
    // every future build on this machine behind a build that is not running.
    if let Err(err) = slot::release_slot(store, &ticket.ticket_id) {
        tracing::error!(error = %err, ticket = %ticket.ticket_id, "failed to release executor slot");
    }
    result
}

fn execute_holding_slot(
    store: &mut MemoryStore,
    seat: &ExecutorSeat,
    ticket: &BuildTicket,
    runner: &dyn BuildRunner,
    lineage: &dyn LineageOracle,
) -> Result<BuildReceipt, String> {
    let started_at = chrono::Utc::now().to_rfc3339();

    let resident = read_slot_state(store, &seat.resident_target)?;
    let scratch = read_slot_state(store, &seat.scratch_target)?;
    let plan = target::plan_target(&ticket.source, &resident, &scratch, lineage)?;
    tracing::info!(
        ticket = %ticket.ticket_id,
        target = %plan.path,
        slot = plan.slot.as_str(),
        clear_first = plan.clear_first,
        reason = %plan.reason,
        "build broker: target plan"
    );

    // Record the target on the slot BEFORE touching it, so that if this process
    // dies mid-build the recovery path knows which dir to quarantine.
    slot::record_slot_target(store, &ticket.ticket_id, &plan.path)?;

    // The target dir is a tracked resource: register it if this is the first
    // time the seat used it.
    let target_resource_id =
        crate::exec_env_ops::ensure_resource_allow_quarantined(store.connection(), &plan.path)?;

    if plan.clear_first {
        clear_target_for_reuse(store, &target_resource_id, &plan, runner)?;
    }

    runner.prepare_checkout(&seat.checkout, &ticket.source)?;
    let run = runner.run(ticket, &seat.checkout, std::path::Path::new(&plan.path))?;

    let mut quarantined_target = None;
    match run.outcome {
        BuildOutcome::Interrupted => {
            // The compiler died with its hands in the target dir. Fence it off:
            // unbindable, unreclaimable, and unusable by the next build until
            // someone clears it (#894 S2c item 4).
            memcore::quarantine_resource(
                store.connection_mut(),
                &target_resource_id,
                &format!(
                    "cargo interrupted (no exit status) while building ticket {}",
                    ticket.ticket_id
                ),
            )
            .map_err(|e| format!("quarantine interrupted target {}: {e}", plan.path))?;
            // Deliberately NOT stamping a generation: we do not know what state
            // this dir reached, so claiming it is "at" the ticket's sha would be
            // a lie the next compatibility check would believe.
            quarantined_target = Some(plan.path.clone());
            tracing::warn!(
                ticket = %ticket.ticket_id,
                target = %plan.path,
                "build broker: interrupted build; target quarantined"
            );
        }
        BuildOutcome::Success | BuildOutcome::Failed => {
            // A failed build still wrote artifacts + fingerprints into this dir,
            // so it still defines the generation.
            target::stamp_generation(
                store,
                &plan.path,
                &TargetGeneration {
                    repo_root: ticket.source.repo_root.clone(),
                    head_sha: ticket.source.head_sha.clone(),
                    ticket_id: ticket.ticket_id.clone(),
                    stamped_at: chrono::Utc::now().to_rfc3339(),
                },
            )?;
        }
    }

    let receipt = BuildReceipt {
        ticket_id: ticket.ticket_id.clone(),
        outcome: run.outcome,
        exit_code: run.exit_code,
        target_path: plan.path.clone(),
        target_slot: plan.slot.as_str().to_string(),
        cleared_target: plan.clear_first,
        quarantined_target,
        env_id: ticket.env_id.clone(),
        source_head_sha: ticket.source.head_sha.clone(),
        started_at,
        finished_at: chrono::Utc::now().to_rfc3339(),
        stdout_tail: run.stdout_tail,
        stderr_tail: run.stderr_tail,
    };
    write_receipt(store, &receipt)?;
    Ok(receipt)
}

/// Wipe a target dir so a foreign-lineage (or quarantined) dir can be reused.
///
/// A quarantined dir goes through `release_quarantine`, which will not return it
/// to `active` unless the wipe actually succeeded — so a half-failed clear leaves
/// it quarantined rather than handing the build a poisoned target.
fn clear_target_for_reuse(
    store: &mut MemoryStore,
    target_resource_id: &str,
    plan: &TargetPlan,
    runner: &dyn BuildRunner,
) -> Result<(), String> {
    let path = PathBuf::from(&plan.path);
    let quarantined = memcore::get_resource(store.connection(), target_resource_id)
        .map_err(|e| e.to_string())?
        .map(|r| r.state == ResourceState::Quarantined)
        .unwrap_or(false);

    if quarantined {
        memcore::release_quarantine(store.connection_mut(), target_resource_id, |_res| {
            runner
                .clear_target(&path)
                .map_err(memcore::MemoryError::InvalidArg)
        })
        .map_err(|e| format!("clear quarantined target {}: {e}", plan.path))?;
    } else {
        let freed = runner.clear_target(&path)?;
        if freed > 0 {
            let _ =
                memcore::record_resource_measurement(store.connection(), target_resource_id, 0, "");
        }
    }
    // The dir is empty: it is virgin again, and must not keep claiming the
    // generation it used to hold.
    target::clear_generation(store, &plan.path)?;
    Ok(())
}

fn read_slot_state(store: &MemoryStore, path: &std::path::Path) -> Result<TargetSlotState, String> {
    let path = path.display().to_string();
    let quarantined =
        memcore::find_resource_by_path(store.connection(), &path, ResourceKind::BuildTarget)
            .map_err(|e| e.to_string())?
            .map(|r| r.state == ResourceState::Quarantined)
            .unwrap_or(false);
    Ok(TargetSlotState {
        generation: target::read_generation(store, &path)?,
        path,
        quarantined,
    })
}

/// Crash recovery (#894 S2c item 4): the slot is held by a build that is no
/// longer running (daemon killed, machine rebooted mid-compile).
///
/// Quarantines the dead holder's target dir FIRST, then releases the slot. Never
/// the other way round: releasing first would let the next ticket grab a target
/// dir that a dead compiler left half-written.
///
/// This is deliberately an explicit operation, not a TTL sweep. We cannot tell
/// "the holder is dead" from "the holder is 40 minutes into a cold build", and
/// guessing wrong means two cargos in one target dir.
pub(crate) fn abandon_stale_slot(store: &mut MemoryStore, reason: &str) -> Result<bool, String> {
    let Some(holder) = slot::current_holder(store)? else {
        return Ok(false);
    };

    if let Some(target_path) = &holder.target_path {
        let resource_id = crate::exec_env_ops::ensure_resource_allow_quarantined(
            store.connection(),
            target_path,
        )?;
        memcore::quarantine_resource(
            store.connection_mut(),
            &resource_id,
            &format!(
                "executor slot abandoned ({reason}); ticket {} (pid {}) was building into this \
                 target and its contents are not trustworthy",
                holder.ticket_id, holder.pid
            ),
        )
        .map_err(|e| format!("quarantine abandoned target {target_path}: {e}"))?;
        tracing::warn!(
            target = %target_path,
            ticket = %holder.ticket_id,
            "build broker: abandoned slot; target quarantined"
        );
    }

    slot::force_release_slot(store)
}
