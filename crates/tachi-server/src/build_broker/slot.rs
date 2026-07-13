//! The machine-unique executor slot (#894 S2c item 3).
//!
//! There is exactly ONE active `cargo` on this machine, and this is the row
//! that says so: `hard_state(namespace = [`BROKER_NS`], key = [`EXECUTOR_SLOT_KEY`])`.
//! The key is a constant, so the row is per-machine (per global store), not
//! per-ticket.
//!
//! Acquisition is `insert_state_if_absent` — a single SQLite
//! `INSERT ... ON CONFLICT DO NOTHING`, so among any number of concurrent
//! callers (threads, processes, daemons) exactly one observes `Acquired` and
//! everyone else observes `Busy`. Same primitive the sticky claim gate uses
//! (`sticky_ops::claim`), for the same reason: a read-then-write would be a
//! race, and the race here costs a poisoned target dir.
//!
//! ## Why the slot is not stealable on a timeout
//!
//! A stale slot (holder died mid-build) is NOT auto-reclaimed after a TTL.
//! Stealing a slot whose `cargo` is still alive would put two compilers in one
//! target dir — the exact failure the serialization exists to prevent, now with
//! a timer as the trigger. Recovery is explicit ([`super::abandon_stale_slot`]):
//! quarantine the target the dead holder was writing into, THEN release. That
//! is fail-safe (a stuck slot blocks builds; a stolen slot corrupts them).

use memcore::MemoryStore;
use serde::{Deserialize, Serialize};

/// `hard_state` namespace for broker singletons.
pub(crate) const BROKER_NS: &str = "build_broker";
/// The one key: this machine's single serialized executor slot.
pub(crate) const EXECUTOR_SLOT_KEY: &str = "executor_slot";

/// Who holds the executor slot right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SlotHolder {
    pub ticket_id: String,
    pub pid: u32,
    pub acquired_at: String,
    /// The target dir this holder is writing into. `None` until the target plan
    /// is chosen; recorded as soon as it is, so that if this process dies
    /// mid-build the recovery path knows exactly which target to quarantine.
    pub target_path: Option<String>,
}

/// Result of trying to take the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlotOutcome {
    Acquired,
    Busy { holder: SlotHolder },
}

/// Take the machine's executor slot for `ticket_id`, or report who has it.
pub(crate) fn acquire_slot(store: &MemoryStore, ticket_id: &str) -> Result<SlotOutcome, String> {
    let holder = SlotHolder {
        ticket_id: ticket_id.to_string(),
        pid: std::process::id(),
        acquired_at: chrono::Utc::now().to_rfc3339(),
        target_path: None,
    };
    let value = serde_json::to_string(&holder).map_err(|e| format!("serialize slot: {e}"))?;
    let won = store
        .insert_state_if_absent(BROKER_NS, EXECUTOR_SLOT_KEY, &value)
        .map_err(|e| format!("acquire executor slot: {e}"))?;
    if won {
        return Ok(SlotOutcome::Acquired);
    }
    match current_holder(store)? {
        Some(holder) => Ok(SlotOutcome::Busy { holder }),
        // Lost the insert race but the row vanished before we read it back (the
        // winner finished that fast). Report busy rather than pretending we hold
        // it: the caller retries, and a retry is cheap. Claiming `Acquired` here
        // would be a second concurrent cargo.
        None => Ok(SlotOutcome::Busy {
            holder: SlotHolder {
                ticket_id: String::new(),
                pid: 0,
                acquired_at: chrono::Utc::now().to_rfc3339(),
                target_path: None,
            },
        }),
    }
}

/// Read the current holder, if any.
pub(crate) fn current_holder(store: &MemoryStore) -> Result<Option<SlotHolder>, String> {
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
    store: &MemoryStore,
    ticket_id: &str,
    target_path: &str,
) -> Result<(), String> {
    let mut holder = current_holder(store)?
        .ok_or_else(|| "executor slot is not held; cannot record its target".to_string())?;
    if holder.ticket_id != ticket_id {
        return Err(format!(
            "executor slot is held by ticket '{}', not '{ticket_id}': refusing to record a \
             target dir on someone else's slot",
            holder.ticket_id
        ));
    }
    holder.target_path = Some(target_path.to_string());
    let value = serde_json::to_string(&holder).map_err(|e| format!("serialize slot: {e}"))?;
    store
        .set_state(BROKER_NS, EXECUTOR_SLOT_KEY, &value)
        .map_err(|e| format!("record executor slot target: {e}"))?;
    Ok(())
}

/// Release the slot. Only the holder may release it: releasing someone else's
/// slot would let a third ticket in while their `cargo` is still running.
/// Returns `false` when there was nothing to release.
pub(crate) fn release_slot(store: &MemoryStore, ticket_id: &str) -> Result<bool, String> {
    match current_holder(store)? {
        None => Ok(false),
        Some(holder) if holder.ticket_id != ticket_id => Err(format!(
            "executor slot is held by ticket '{}', not '{ticket_id}': refusing to release \
             someone else's slot (that would admit a second concurrent cargo)",
            holder.ticket_id
        )),
        Some(_) => store
            .delete_state(BROKER_NS, EXECUTOR_SLOT_KEY)
            .map_err(|e| format!("release executor slot: {e}")),
    }
}

/// Force the slot open, whoever holds it. The ONLY caller is the explicit
/// crash-recovery path ([`super::abandon_stale_slot`]), which quarantines the
/// dead holder's target dir *first*.
pub(crate) fn force_release_slot(store: &MemoryStore) -> Result<bool, String> {
    store
        .delete_state(BROKER_NS, EXECUTOR_SLOT_KEY)
        .map_err(|e| format!("force-release executor slot: {e}"))
}
