//! Handoff memos (#157-era) — **deprecated**, see #1016 ruling below.
//!
//! `handoff_leave` / `handoff_check` predate two purpose-built replacements
//! that have since split its two use cases cleanly:
//!
//! - **Short agent-to-agent memo, read-once** → `sticky_ops` (#964,
//!   `tachi_memory(action='sticky_leave'|'sticky_check')`). Atomic-claim
//!   delivery, TTL, addressing — everything `handoff_ops::pending`'s
//!   read-then-write acknowledge loop only approximated.
//! - **Structured baton for a resumed/handed-off task** →
//!   `orchestrator_ops::HandoffPacket` (`tachi_orchestrator(action='handoff_write'|'handoff_read')`).
//!   Carries objective/current_state/completed_steps/remaining_steps/
//!   files_touched/commands_run/tests_run/known_blockers/next_action —
//!   the shape a resuming session actually needs, keyed by `task_id`
//!   rather than a loosely-addressed "next agent".
//!
//! **Ruling (#1016, owner-ratified 2026-07-11): `handoff_ops` is deprecated,
//! not deleted.** Existing callers keep working; `handoff_leave` /
//! `handoff_check` / `tachi_handoff` responses carry a `deprecated` field
//! pointing at the replacement. `promote_issue` (memo → GitHub issue) has
//! no direct replacement yet and is unaffected by this ruling. Deletion is a
//! later, separately-audited cut (#757-style) once in-tree/production
//! callers are confirmed migrated — this pass does not remove any code path.
mod gc;
mod handlers;
mod identity;
mod issue;
mod memo;
mod pending;

#[cfg(test)]
mod tests;

pub(super) const HANDOFF_PATH: &str = "/handoff";
pub(super) const HANDOFF_MEMORY_LIMIT: usize = 50;
pub(super) const HANDOFF_DB_LIMIT: usize = 500;

/// Deprecation pointer surfaced in `handoff_leave`/`handoff_check` responses
/// (#1016 ruling) — leave/check split into two purpose-built replacements.
pub(crate) const DEPRECATION_NOTICE: &str = "handoff_ops leave/check is deprecated (#1016): use tachi_memory(action='sticky_leave'|'sticky_check') for a short agent-to-agent note, or tachi_orchestrator(action='handoff_write'|'handoff_read') for a structured task baton.";

pub(crate) use gc::gc_expired_handoff_memories;
pub(crate) use handlers::{handle_handoff_check, handle_handoff_leave};
pub(crate) use issue::handle_handoff_promote_issue;
pub(crate) use pending::list_pending_handoffs_for_briefing;

#[cfg(test)]
use crate::server_state::{HandoffMemo, MemoryServer};
#[cfg(test)]
use crate::tool_params::{HandoffCheckParams, HandoffLeaveParams, HandoffPromoteIssueParams};
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
use memo::memo_from_entry;
#[cfg(test)]
use pending::{pending_handoff_entries, supersede_pending_handoffs, upsert_acknowledged_entry};
#[cfg(test)]
use serde_json::json;
