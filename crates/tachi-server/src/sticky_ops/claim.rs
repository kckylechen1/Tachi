//! Atomic read-once claim gate for stickies.
//!
//! Frozen semantics (#964, owner-ratified 2026-07-11): marking a sticky read
//! must be a compare-and-set so that under N concurrent readers exactly ONE
//! delivery happens. A naive read-then-write on the `memories` row (the same
//! shape as `handoff_ops::pending::upsert_acknowledged_entry`, which is
//! last-write-wins) is explicitly rejected for this reason.
//!
//! The claim gate lives on the store's separate `hard_state` KV table
//! (namespace `STICKY_CLAIM_NAMESPACE`, key = sticky id), using
//! `MemoryStore::insert_state_if_absent` — a single SQLite
//! `INSERT ... ON CONFLICT DO NOTHING`, so exactly one caller among any
//! number of concurrent `try_claim_sticky` calls observes `Ok(true)`. The
//! memory row itself (content/display) is a separate, non-atomic write that
//! happens only after a successful claim — it never participates in the
//! race.

use memcore::MemoryStore;

#[cfg(test)]
use crate::MemoryServer;

pub(super) const STICKY_CLAIM_NAMESPACE: &str = "sticky_claim";

/// Attempt to atomically claim a sticky for delivery. Returns `Ok(true)` iff
/// this call is the one that won the claim (i.e. the sticky had not already
/// been claimed). Safe to call from many concurrent callers against the same
/// `sticky_id` — the underlying `INSERT ... ON CONFLICT DO NOTHING` guarantees
/// exactly one winner.
pub(super) fn try_claim_sticky(
    store: &mut MemoryStore,
    sticky_id: &str,
    claimed_by: Option<&str>,
) -> Result<bool, String> {
    let value_json = serde_json::json!({
        "claimed_by": claimed_by,
        "claimed_at": chrono::Utc::now().to_rfc3339(),
    })
    .to_string();
    store
        .insert_state_if_absent(STICKY_CLAIM_NAMESPACE, sticky_id, &value_json)
        .map_err(|e| format!("Failed to claim sticky {sticky_id}: {e}"))
}

/// Whether `sticky_id` has already been claimed (read-only check; does not
/// itself claim). Test-only helper for asserting on the CAS gate directly;
/// production code reads claimed/expired status from the memory row's own
/// metadata mirror (`memo::sticky_row_is_unread`), not this table.
#[cfg(test)]
pub(super) fn sticky_is_claimed(store: &MemoryServer, sticky_id: &str) -> bool {
    store
        .with_global_store_read(|store| {
            Ok(store
                .get_state_kv(STICKY_CLAIM_NAMESPACE, sticky_id)
                .map_err(|e| format!("{e}"))?
                .is_some())
        })
        .unwrap_or(false)
}
