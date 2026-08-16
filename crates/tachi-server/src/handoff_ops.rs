//! Handoff memos (#157-era) — **mostly retired**, see #1099 below.
//!
//! `handoff_leave` / `handoff_check` predated two purpose-built replacements
//! that split its two use cases cleanly:
//!
//! - **Same-host agent advisory message** → `tachi_a2a(action='respond')`.
//!   Delivery is bound to explicit admitted AgentIdentity rows and receipts.
//! - **Structured baton for a resumed/handed-off task** →
//!   `orchestrator_ops::HandoffPacket` (`tachi_orchestrator(action='handoff_write'|'handoff_read')`).
//!   Carries objective/current_state/completed_steps/remaining_steps/
//!   files_touched/commands_run/tests_run/known_blockers/next_action —
//!   the shape a resuming session actually needs, keyed by `task_id`
//!   rather than a loosely-addressed "next agent".
//!
//! **#1016 first deprecated this module without deleting anything.** **#1099
//! (owner-ratified 2026-07-17) retires the write/read cycle for real**: the
//! `handoff_leave`/`handoff_check` routes, `tachi_handoff`'s 'leave'/'check'
//! actions, the pending-memo briefing projection, and the dedicated
//! handoff-memory GC branch are all gone — caller sweep found zero live
//! external callers (repo-wide grep: only in-crate tests called them; the
//! OpenClaw experimental passthrough that referenced the raw names is
//! default-off and is retired alongside them, see
//! `integrations/openclaw/index.ts`).
//!
//! `promote_issue` is the one documented capability without a replacement
//! (#1037 is the deferred future replacement for "explicit issue/global
//! handoff publication") and survives as the sole action `tachi_handoff`
//! still supports. It only *reads* pre-existing `handoff:<id>` memory
//! entries — with the writer gone, it becomes a wind-down capability for
//! whatever handoff memos already exist rather than a going concern, which
//! the owner ruling accepted explicitly rather than inventing a new writer
//! here (that would be scope creep beyond #1099's bounded retirement wave).
//!
//! **Legacy-row data policy (required evidence, #1099): retain read-only.**
//! Existing persisted `handoff:<id>` memory entries are ordinary
//! `MemoryEntry` rows (category="handoff") — they are not deleted, migrated,
//! or specially reaped by this change. They remain fully readable/searchable
//! (`tachi_memory(search)`, `get_memory`) and remain promotable via
//! `promote_issue`. The only thing that goes away is the dedicated 30-day
//! GC sweep that used to force-delete acknowledged/promoted/superseded rows
//! (`gc_expired_handoff_memories`, ex-`gc.rs`) — since nothing can produce
//! "acknowledged"/"superseded" status anymore (those came from the retired
//! `handoff_check`/`handoff_leave` write paths), that branch had nothing
//! left to do except delete `promoted` rows, which is not a disposition
//! this pass invents. Never silently orphaned: the rows stay visible, just
//! no longer specially swept.
mod identity;
mod issue;
mod memo;

#[cfg(test)]
mod tests;

// #1099: `HANDOFF_PATH` is now only consumed by the test-only `test_entry`
// helper (the non-test writer that used it, `memo_to_memory_entry`, was
// retired along with `handoff_leave`) — gate it so a normal build doesn't
// carry a dead `pub(super)` const.
#[cfg(test)]
pub(super) const HANDOFF_PATH: &str = "/handoff";

pub(crate) use issue::handle_handoff_promote_issue;

#[cfg(test)]
use crate::server_state::{HandoffMemo, MemoryServer};
#[cfg(test)]
use crate::tool_params::HandoffPromoteIssueParams;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use identity::fallback_agent_id;
#[cfg(test)]
use issue::{
    existing_issue_url, promote_handoff_issue_with_client, revert_promoting_entry,
    upsert_promoting_entry,
};
#[cfg(test)]
use memcore::{MemoryEntry, MemoryStore};
#[cfg(test)]
use serde_json::json;
