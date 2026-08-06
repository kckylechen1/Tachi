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
//! - [`ticket`] — the immutable request (write-once in `hard_state`) plus the
//!   queue's terminal states (dead letter / cancelled).
//! - [`repo`] — repo identity: the queue is machine-wide, a seat is per-repo, so
//!   a seat only ever drains its own repo's tickets.
//! - [`slot`] — the machine-unique executor slot (`INSERT ... ON CONFLICT DO
//!   NOTHING`, so exactly one winner among any number of concurrent callers).
//! - [`target`] — generations + the compatibility plan (the poisoning defense).
//! - [`runner`] — git checkout / subprocess / wipe, behind a trait.
//!
//! ## The queue is not a trap
//!
//! A ticket the seat cannot run (dirty checkout, sha that does not resolve, git
//! failure) is **booked as a failed attempt and skipped**, and after
//! [`ticket::MAX_TICKET_ATTEMPTS`] it is dead-lettered into a terminal state
//! that [`run_next`] never picks up again. Round-1 propagated such an error with
//! the ticket still pending, so the next drain took the same doomed ticket, hit
//! the same error, and every ticket behind it starved forever — a head-of-line
//! block with no exit and no cancel. Now there are both: the bounded retry, and
//! [`cancel_queued_ticket`].
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

mod ensure_resource;
pub mod repo;
pub mod runner;
pub mod slot;
pub mod target;
pub mod ticket;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use ensure_resource::ensure_resource_allow_quarantined;

use std::path::PathBuf;

use memcore::{MemoryStore, ResourceKind, ResourceState};
use serde::{Deserialize, Serialize};

use runner::{BuildOutcome, BuildRunner};
use slot::SlotOutcome;
use target::{LineageOracle, TargetGeneration, TargetPlan, TargetSlotState};
use ticket::{BuildTicket, CancelOutcome, TicketState, TicketStatus};

/// `hard_state` namespace for build receipts; key = ticket id.
pub const RECEIPT_NS: &str = "build_receipt";

/// The machine's one build executor seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorSeat {
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
pub struct BuildReceipt {
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
    /// `hard_state` TTL (#1342 follow-up): a receipt is done being useful the
    /// instant it is written (it is an immutable, already-consumed result),
    /// so every new write stamps `now + 90d` here — `memcore::reap_expired_state`
    /// (which reads this exact top-level JSON field) then owns cleanup.
    /// `#[serde(default)]` lets a pre-TTL receipt already on disk decode as
    /// `""`, which `reap_expired_state` treats as un-parseable and therefore
    /// never reaps (fail-closed, not a retroactive expiry); `background.rs`'s
    /// idempotent backfill is what actually assigns those rows a real TTL.
    #[serde(default)]
    pub expires_at: String,
}

/// Read a ticket's receipt, if the build has already run.
pub fn load_receipt(store: &MemoryStore, ticket_id: &str) -> Result<Option<BuildReceipt>, String> {
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

/// The queue: submitted tickets that are still runnable — no receipt, not
/// terminal (dead-lettered or cancelled) — oldest first.
///
/// `repo` is the seat's repo identity (see [`repo::repo_identity`]); `None`
/// means "every repo on this machine", which only the machine-wide views want.
/// A seat MUST pass its own repo: the ticket store is machine-wide, and handing
/// repo A's ticket to repo B's seat means checking A's sha out inside B's tree
/// (#894 S2c round-2).
pub fn pending_tickets(
    store: &MemoryStore,
    repo: Option<&str>,
) -> Result<Vec<BuildTicket>, String> {
    let mut pending = Vec::new();
    for t in ticket::list_tickets(store)? {
        if !repo::same_repo(&t.source.repo_root, repo) {
            continue;
        }
        if load_receipt(store, &t.ticket_id)?.is_some() {
            continue;
        }
        if ticket::ticket_state(store, &t.ticket_id)?.is_terminal() {
            continue;
        }
        pending.push(t);
    }
    Ok(pending)
}

/// What one drain step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainStep {
    /// The build that ran, if one did.
    pub receipt: Option<BuildReceipt>,
    /// Tickets this step took, failed, and booked — the ones it stepped OVER to
    /// get to the receipt. A ticket that has now burned its attempts appears
    /// here with `state == Failed` (a dead letter).
    pub failures: Vec<TicketStatus>,
    /// The slot was held by someone else; nothing was attempted.
    pub slot_busy: bool,
}

impl DrainStep {
    fn nothing() -> Self {
        DrainStep {
            receipt: None,
            failures: Vec::new(),
            slot_busy: false,
        }
    }
}

/// Take the oldest runnable ticket **for this seat's repo** and run it.
///
/// Failure handling is the round-2 fix. A ticket that fails for any reason other
/// than "the slot is busy" — a dirty seat checkout, a sha git cannot resolve, a
/// target that cannot be cleared — is **booked** (attempt +1, dead-lettered once
/// it hits [`ticket::MAX_TICKET_ATTEMPTS`]) and this call moves on to the next
/// ticket. It does not propagate the error with the ticket left at the head of
/// the queue, which is what made round-1's queue a trap: one bad ticket and no
/// build on the machine ever ran again.
///
/// `slot_busy` is not a failure and is never booked against a ticket: nothing
/// was attempted, and the caller simply comes back later.
pub fn run_next(
    store: &mut MemoryStore,
    seat: &ExecutorSeat,
    repo: &str,
    runner: &dyn BuildRunner,
    lineage: &dyn LineageOracle,
) -> Result<DrainStep, String> {
    let mut step = DrainStep::nothing();

    loop {
        let queue = pending_tickets(store, Some(repo))?;
        // Skip the ones we already failed in THIS call: their status row is
        // written, but a ticket below the attempt cap is still `Queued` and would
        // be handed back to us forever by the line above.
        let Some(t) = queue
            .into_iter()
            .find(|t| !step.failures.iter().any(|f| f.ticket_id == t.ticket_id))
        else {
            return Ok(step);
        };

        match execute_ticket(store, seat, &t, runner, lineage) {
            Ok(receipt) => {
                step.receipt = Some(receipt);
                return Ok(step);
            }
            Err(err) if err.starts_with(SLOT_BUSY) => {
                tracing::debug!(error = %err, "build broker: executor slot busy, deferring");
                step.slot_busy = true;
                return Ok(step);
            }
            Err(err) => {
                let status = ticket::record_failed_attempt(store, &t.ticket_id, &err)?;
                match status.state {
                    TicketState::Failed => tracing::error!(
                        ticket = %t.ticket_id,
                        attempts = status.attempts,
                        error = %err,
                        "build broker: ticket DEAD-LETTERED after {} failed attempts; it will \
                         never be picked up again (tachi build status shows it)",
                        status.attempts
                    ),
                    _ => tracing::warn!(
                        ticket = %t.ticket_id,
                        attempts = status.attempts,
                        error = %err,
                        "build broker: ticket failed; will retry (bounded)"
                    ),
                }
                step.failures.push(status);
                // …and on to the next ticket. A doomed ticket must not starve
                // the queue behind it.
            }
        }
    }
}

/// Cancel a queued ticket (`tachi build cancel`).
///
/// Fail-closed on the two states where "cancel" would be a lie:
/// - the ticket already has a **receipt** → the build ran; there is nothing to
///   cancel, and pretending otherwise would leave a cancelled marker on a real
///   result;
/// - the ticket is **holding the executor slot** → its cargo may be alive right
///   now. Cancelling the queue entry would not stop it, and would let the next
///   ticket into a target dir a live compiler is writing to. Killing that build
///   is `build abandon`'s job, and only once the process is actually dead.
pub fn cancel_queued_ticket(
    store: &MemoryStore,
    ticket_id: &str,
    reason: &str,
) -> Result<CancelOutcome, String> {
    if load_receipt(store, ticket_id)?.is_some() {
        return Err(format!(
            "build ticket '{ticket_id}' already has a receipt: the build ran, so there is nothing \
             to cancel (#894 S2c)"
        ));
    }
    if let Some(holder) = slot::current_holder(store)? {
        if holder.ticket_id == ticket_id {
            return Err(format!(
                "build ticket '{ticket_id}' is holding the executor slot (pid {}): its cargo may \
                 be running right now. Cancelling the queue entry would not stop it. If that \
                 process is dead, use `tachi build abandon` — it quarantines the target dir the \
                 dead build was writing into BEFORE freeing the slot (#894 S2c)",
                holder.pid
            ));
        }
    }
    ticket::cancel_ticket(store, ticket_id, reason)
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
pub fn execute_ticket(
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
    let target_resource_id = ensure_resource_allow_quarantined(store.connection_mut(), &plan.path)?;

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
        expires_at: (chrono::Utc::now() + chrono::Duration::days(90)).to_rfc3339(),
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
            let _ = memcore::record_resource_measurement(
                store.connection_mut(),
                target_resource_id,
                0,
                "",
            );
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
pub fn abandon_stale_slot(store: &mut MemoryStore, reason: &str) -> Result<bool, String> {
    let Some(holder) = slot::current_holder(store)? else {
        return Ok(false);
    };

    if let Some(target_path) = &holder.target_path {
        let resource_id = ensure_resource_allow_quarantined(store.connection_mut(), target_path)?;
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
