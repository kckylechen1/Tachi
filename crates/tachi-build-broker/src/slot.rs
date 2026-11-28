//! The machine-unique executor slot (#894 S2c item 3).
//!
//! There is exactly ONE active `cargo` on this machine, and this is the row
//! that says so: `hard_state(namespace = [`BROKER_NS`], key = [`EXECUTOR_SLOT_KEY`])`.
//! The key is a constant, so the row is per-machine (per global store), not
//! per-ticket.
//!
//! Acquisition is `insert_state_if_absent` — a single SQLite
//! `INSERT ... ON CONFLICT DO NOTHING` — inside the same write transaction
//! that reads the ticket's receipt and queue state, so among any number of
//! concurrent callers (threads, processes, daemons) exactly one observes
//! [`Claim::Held`] and everyone else observes [`Claim::Busy`]. A read-then-write
//! would be a race, and the race here costs a poisoned target dir.
//!
//! ## Why the slot is not stealable on a timeout
//!
//! A stale slot (holder died mid-build) is NOT auto-reclaimed after a TTL.
//! Stealing a slot whose `cargo` is still alive would put two compilers in one
//! target dir — the exact failure the serialization exists to prevent, now with
//! a timer as the trigger. Recovery is explicit ([`super::abandon_stale_slot`]):
//! quarantine the target the dead holder was writing into, THEN release. That
//! is fail-safe (a stuck slot blocks builds; a stolen slot corrupts them).

use super::BuildReceipt;
use crate::ticket::{self, StateReader, TicketState};
use memcore::store::state::StateTransaction;
use memcore::MemoryStore;
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};

/// `hard_state` namespace for broker singletons.
pub(crate) const BROKER_NS: &str = "build_broker";
/// The one key: this machine's single serialized executor slot.
pub(crate) const EXECUTOR_SLOT_KEY: &str = "executor_slot";

/// Who holds the executor slot right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotHolder {
    pub ticket_id: String,
    pub pid: u32,
    pub acquired_at: String,
    /// Unique per claim, so even the same ticket and PID cannot be confused
    /// with a later acquisition during crash recovery.
    #[serde(default)]
    pub acquisition_id: String,
    /// The target dir this holder is writing into. `None` until the target plan
    /// is chosen; recorded as soon as it is, so that if this process dies
    /// mid-build the recovery path knows exactly which target to quarantine.
    pub target_path: Option<String>,
}

/// Whether a ticket may build now, decided together with the state it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Claim {
    /// This process holds the executor slot for the ticket and acquisition.
    Held(SlotHolder),
    /// The slot is taken; nothing was attempted for the ticket.
    Busy { holder: SlotHolder },
    /// The build already ran. Receipts are write-once, so running again would
    /// produce a result nobody can record.
    AlreadyBuilt(BuildReceipt),
    /// The ticket was cancelled or dead-lettered after the caller listed it.
    LeftQueue(TicketState),
}

/// Take the executor slot for a ticket that is still allowed to build.
///
/// The ticket's receipt, its queue state and the slot are read under the same
/// write lock as the insert. A cancellation that commits first is seen here
/// and the ticket is not built; one that comes second sees the slot held and
/// is refused. Checked separately, a cancel could land between the executor's
/// queue read and its slot insert, leaving a cancelled marker on a real build.
pub(crate) fn claim_for_execution(
    store: &mut MemoryStore,
    ticket_id: &str,
) -> Result<Claim, String> {
    let tx = store
        .begin_state_transaction(TransactionBehavior::Immediate)
        .map_err(|e| format!("begin executor claim: {e}"))?;
    let claim = claim_in(&tx, ticket_id)?;
    if matches!(claim, Claim::Held(_)) {
        tx.commit()
            .map_err(|e| format!("commit executor claim: {e}"))?;
    }
    Ok(claim)
}

fn claim_in(tx: &StateTransaction<'_>, ticket_id: &str) -> Result<Claim, String> {
    if let Some(receipt) = super::load_receipt(tx, ticket_id)? {
        return Ok(Claim::AlreadyBuilt(receipt));
    }
    if let Some(status) = ticket::load_status(tx, ticket_id)? {
        if status.state.is_terminal() {
            return Ok(Claim::LeftQueue(status.state));
        }
    }
    let holder = SlotHolder {
        ticket_id: ticket_id.to_string(),
        pid: std::process::id(),
        acquired_at: jiff::Timestamp::now().to_string(),
        acquisition_id: uuid::Uuid::new_v4().to_string(),
        target_path: None,
    };
    let value = serde_json::to_string(&holder).map_err(|e| format!("serialize slot: {e}"))?;
    if tx
        .insert_state_if_absent(BROKER_NS, EXECUTOR_SLOT_KEY, &value)
        .map_err(|e| format!("acquire executor slot: {e}"))?
    {
        return Ok(Claim::Held(holder));
    }
    // The write lock is held, so the row that won the insert cannot vanish
    // before it is read back.
    Ok(Claim::Busy {
        holder: holder_in(tx)?.ok_or("executor slot row vanished inside its claim")?,
    })
}

/// Read the current holder, if any, through a state reader.
pub(crate) fn holder_in(source: &impl StateReader) -> Result<Option<SlotHolder>, String> {
    let row = source
        .state(BROKER_NS, EXECUTOR_SLOT_KEY)
        .map_err(|e| format!("read executor slot: {e}"))?;
    match row {
        None => Ok(None),
        Some((json, _version)) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("decode executor slot: {e}")),
    }
}

/// Read the current holder, if any.
pub fn current_holder(store: &MemoryStore) -> Result<Option<SlotHolder>, String> {
    let row = store
        .get_state_kv(BROKER_NS, EXECUTOR_SLOT_KEY)
        .map_err(|e| format!("read executor slot: {e}"))?;
    match row {
        None => Ok(None),
        Some((json, _version)) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("decode executor slot: {e}")),
    }
}

/// Record which target dir the holder is about to write into. Only the holder
/// may do this — a non-holder writing here would mislabel the recovery path's
/// quarantine target.
pub(crate) fn record_slot_target(
    store: &mut MemoryStore,
    expected: &SlotHolder,
    target_path: &str,
) -> Result<(), String> {
    let tx = store
        .begin_state_transaction(TransactionBehavior::Immediate)
        .map_err(|e| format!("begin slot-target update: {e}"))?;
    let mut holder = holder_in(&tx)?
        .ok_or_else(|| "executor slot is not held; cannot record its target".to_string())?;
    if &holder != expected {
        return Err(format!(
            "executor slot acquisition '{}' no longer belongs to ticket '{}'; refusing to update it",
            expected.acquisition_id, expected.ticket_id
        ));
    }
    holder.target_path = Some(target_path.to_string());
    let value = serde_json::to_string(&holder).map_err(|e| format!("serialize slot: {e}"))?;
    tx.set_state(BROKER_NS, EXECUTOR_SLOT_KEY, &value)
        .map_err(|e| format!("record executor slot target: {e}"))?;
    tx.commit()
        .map_err(|e| format!("commit executor slot target: {e}"))?;
    Ok(())
}

/// Release the slot. Only the exact acquisition may release it.
fn same_acquisition(left: &SlotHolder, right: &SlotHolder) -> bool {
    left.ticket_id == right.ticket_id
        && left.pid == right.pid
        && left.acquired_at == right.acquired_at
        && left.acquisition_id == right.acquisition_id
}

pub(crate) fn release_slot(store: &mut MemoryStore, expected: &SlotHolder) -> Result<bool, String> {
    let tx = store
        .begin_state_transaction(TransactionBehavior::Immediate)
        .map_err(|e| format!("begin executor release: {e}"))?;
    match holder_in(&tx)? {
        None => Ok(false),
        Some(holder) if !same_acquisition(&holder, expected) => Err(format!(
            "executor slot is held by ticket '{}', acquisition '{}', not the caller's '{}': refusing to release it",
            holder.ticket_id, holder.acquisition_id, expected.acquisition_id
        )),
        Some(_) => {
            let deleted = tx
                .delete_state(BROKER_NS, EXECUTOR_SLOT_KEY)
                .map_err(|e| format!("release executor slot: {e}"))?;
            if deleted {
                tx.commit()
                    .map_err(|e| format!("commit executor release: {e}"))?;
            }
            Ok(deleted)
        }
    }
}
