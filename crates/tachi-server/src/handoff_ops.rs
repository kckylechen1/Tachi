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
use memo::memo_from_entry;
#[cfg(test)]
use memcore::{MemoryEntry, MemoryStore};
#[cfg(test)]
use pending::{pending_handoff_entries, supersede_pending_handoffs, upsert_acknowledged_entry};
#[cfg(test)]
use serde_json::json;
