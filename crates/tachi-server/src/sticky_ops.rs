//! Sticky notes: read-once agent-to-agent ephemeral memos (#964).
//!
//! Frozen semantics (owner-ratified 2026-07-11):
//! 1. `leave(text, to?, ttl_days=7)` stores an ephemeral memory row in a
//!    `/sticky/` bucket via the normal provenance/scrub pipeline.
//! 2. Read-once via ATOMIC CLAIM: delivery (briefing inclusion or explicit
//!    `check`) races against concurrent callers on a `hard_state`
//!    compare-and-set (`claim::try_claim_sticky`) — exactly one delivery
//!    happens no matter how many concurrent readers ask. Naive
//!    read-then-write (the shape `handoff_ops::pending::upsert_acknowledged_entry`
//!    uses, last-write-wins) is explicitly rejected for this reason.
//! 3. Addressing: absent `to` = leader/main session ONLY. Worker seats never
//!    consume unaddressed stickies; a named seat sees only stickies
//!    addressed to that seat.
//! 4. Delivery: unread stickies for the caller surface at the TOP of the
//!    briefing output and are claimed (read-once) by that inclusion. A
//!    caller with no seat identity is treated as leader.
//! 5. `include_read=true` shows the archive (claimed + expired), read-only,
//!    never claims anything.
//! 6. TTL: unread past `ttl_days` -> archived, never surfaces again except
//!    via `include_read=true`.
//! 7. Replaces the retired kanban trio / handoff_leave semantics for this use
//!    case — no permanent board CRUD.

mod claim;
mod gc;
mod handlers;
mod identity;
mod memo;
mod pending;

#[cfg(test)]
mod tests;

pub(crate) const STICKY_PATH: &str = "/sticky";
pub(crate) const STICKY_DB_LIMIT: usize = 500;

pub(crate) use gc::gc_expired_sticky_memories;
pub(crate) use handlers::{
    handle_sticky_check, handle_sticky_leave, StickyCheckInput, StickyLeaveInput,
};
pub(crate) use pending::claim_unread_stickies_for_briefing;

#[cfg(test)]
use claim::{sticky_is_claimed, try_claim_sticky};
#[cfg(test)]
use memo::{
    sticky_from_entry, sticky_row_is_unread, sticky_ttl_expired, sticky_visible_to, StickyMemo,
};
#[cfg(test)]
use pending::{all_sticky_entries, list_or_claim_stickies};
