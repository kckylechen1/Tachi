//! Sticky notes: read-once agent-to-agent ephemeral memos (#964).
//!
//! Frozen semantics (owner-ratified 2026-07-11):
//! 1. `leave(text, to?, ttl_days=7)` stores an ephemeral memory row in a
//!    `/sticky/` bucket via the normal provenance/scrub pipeline.
//! 2. Read-once via ATOMIC CLAIM: delivery (briefing inclusion or explicit
//!    `check`) races against concurrent callers on a `hard_state`
//!    compare-and-set (`claim::try_claim_sticky`) — exactly one delivery
//!    happens no matter how many concurrent readers ask. Naive
//!    read-then-write (the shape the old, #1099-retired
//!    `handoff_ops::pending::upsert_acknowledged_entry` used, last-write-wins)
//!    is explicitly rejected for this reason.
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
//!
//! Crash-safety (CP3, round-2 per codex final review of #964/PR #1003 — this
//! replaces an earlier, overclaiming version of this note): the durable
//! `hard_state` CAS claim (`claim::try_claim_sticky`) and the memory-row
//! status mirror (`pending::mark_claimed`) are two non-transactional writes,
//! and there is a real residual crash window between them and the response
//! reaching the caller:
//!
//! 1. `try_claim_sticky` commits to SQLite — this is the durable, atomic
//!    "who won" decision (CP1's single-winner guarantee), and it is
//!    unconditionally true from this point on.
//! 2. The winning sticky's content is queued into this request's in-memory
//!    `delivered` vec, and (best-effort) `mark_claimed` mirrors the claim
//!    onto the memory row.
//! 3. The MCP/briefing response carrying `delivered` is serialized and
//!    returned to the caller.
//!
//! If the process dies anywhere between step 1 and the caller actually
//! receiving the response in step 3 (including if `mark_claimed` itself never
//! ran), the claim is durably committed (the `hard_state` CAS row exists) but
//! the text was never actually delivered to anyone. Depending on exactly
//! where in that window the crash lands, the memory row's own `status`
//! metadata may or may not have been updated to `"claimed"` before the
//! crash — if `mark_claimed` never ran, the row still reads `"unread"` in
//! `include_read`/GC views (per the comment on `mark_claimed`'s error
//! branch) even though the CAS has already permanently decided a winner.
//! Either way, no caller ever saw its body. Concretely: **a crash in this
//! window durably consumes the sticky's one-and-only delivery without any
//! live caller having received its content** — this is a real,
//! non-hypothetical residual, not eliminated by the CAS (the CAS only
//! prevents *two* callers from both thinking they delivered the same
//! sticky; it does not guarantee delivery reached either).
//!
//! Recovery escape (round-3 narrowing, codex final review of #964/PR #1003,
//! BUG CP3 — the round-2 note above overclaimed this): the row is never
//! deleted, so it remains visible via `action='check', include_read=true`
//! regardless of its claimed/unread status (see
//! `pending::list_or_claim_stickies`'s `include_read` branch, which does not
//! filter on status/archived) — BUT that branch still runs every row through
//! `sticky_visible_to(&memo, agent_id)` (same recipient-visibility gate
//! delivery itself uses; see `memo.rs`), so **only the identity a sticky was
//! actually addressed to (or the leader, for an unaddressed/broadcast
//! sticky) can recover it this way** — a leader querying `include_read` does
//! NOT see a sticky that was `to:`-addressed to a worker seat; only that
//! worker's own seat identity (`agent_id` param or `TACHI_AGENT_SEAT`) can.
//! There is no leader-sees-all override today (that would be an
//! information-flow change, out of scope for this fix — noted as a possible
//! follow-up, not built here). The archive is also bounded, and the two caps
//! sit on opposite sides of the visibility filter (round-4 correction, codex
//! review of #964/PR #1003 — the round-3 note above wrongly claimed both
//! derive from the querying identity's visible slice): `all_sticky_entries`
//! caps the underlying store scan to the newest `STICKY_DB_LIMIT` (500) rows
//! table-wide, BEFORE `sticky_visible_to` filtering runs (see
//! `pending.rs`'s `list_or_claim_stickies`, which calls `all_sticky_entries`
//! first and filters per-row after) — a sticky older than the newest 500
//! rows in the *entire* `/sticky` bucket (any recipient) is dropped before
//! visibility is even considered. Only the second cap, `include_read`'s own
//! returned page (at most 50 rows, no pagination), is a cap on the
//! post-filter, identity-visible slice. Within those bounds, and for the
//! identity a sticky is actually visible to, an operator/agent can read the
//! sticky's text back out of the archive and hand-deliver / re-`sticky_leave`
//! it — but that recovered text is the SCRUBBED form (`scrub_sticky_text_for_read`
//! masks secrets and strips think-tags at the same row-load choke point
//! delivery uses; see `pending.rs`), not the original raw text that was
//! passed to `sticky_leave` (which is itself already write-time scrubbed, so
//! in practice the two usually coincide, but the archive path does not
//! promise or return unredacted original content). There is no automatic
//! re-delivery; this module does not (and, per this adjudication, will not)
//! add write-ahead journaling or a cross-store transaction to close the
//! crash window — the residual is documented, not eliminated, and the
//! visibility-scoped, capped, scrubbed archive above is the intended manual
//! recovery path.

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
pub(crate) use identity::resolve_caller_agent_id;
pub(crate) use pending::claim_unread_stickies_for_briefing;

#[cfg(test)]
use claim::{sticky_is_claimed, try_claim_sticky};
#[cfg(test)]
use memo::{
    sticky_from_entry, sticky_row_is_unread, sticky_ttl_expired, sticky_visible_to, StickyMemo,
};
#[cfg(test)]
use pending::{all_sticky_entries, list_or_claim_stickies, mark_claimed};
