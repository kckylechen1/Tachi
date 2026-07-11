use chrono::Utc;
use serde_json::json;

use crate::MemoryServer;

use super::identity::resolve_from_agent;
use super::memo::{sticky_to_memory_entry, StickyMemo};
use super::pending::list_or_claim_stickies;

const DEFAULT_TTL_DAYS: u32 = 7;

pub(crate) struct StickyLeaveInput {
    pub(crate) text: String,
    pub(crate) to: Option<String>,
    pub(crate) ttl_days: Option<u32>,
}

pub(crate) async fn handle_sticky_leave(
    server: &MemoryServer,
    input: StickyLeaveInput,
) -> Result<String, String> {
    if input.text.trim().is_empty() {
        return Err("text is required and must be non-empty when action='sticky_leave'".into());
    }
    let from_agent = resolve_from_agent(server);
    let ttl_days = input.ttl_days.unwrap_or(DEFAULT_TTL_DAYS).max(1);

    let memo = StickyMemo {
        id: uuid::Uuid::new_v4().to_string(),
        from_agent: from_agent.clone(),
        to: input.to.clone(),
        text: input.text.clone(),
        created_at: Utc::now().to_rfc3339(),
        ttl_days,
    };

    let entry = sticky_to_memory_entry(server, &memo);
    server.with_global_store(|store| store.upsert(&entry).map_err(|e| format!("{e}")))?;

    serde_json::to_string(&json!({
        "status": "sticky_left",
        "sticky_id": memo.id,
        "from_agent": from_agent,
        "to": memo.to,
        "ttl_days": ttl_days,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) struct StickyCheckInput {
    pub(crate) agent_id: Option<String>,
    pub(crate) include_read: bool,
    pub(crate) limit: Option<usize>,
}

pub(crate) async fn handle_sticky_check(
    server: &MemoryServer,
    input: StickyCheckInput,
) -> Result<String, String> {
    let agent_id = input.agent_id.as_deref();
    let limit = input.limit.unwrap_or(10);
    let rows = list_or_claim_stickies(server, agent_id, input.include_read, limit)?;

    serde_json::to_string(&json!({
        "count": rows.len(),
        "include_read": input.include_read,
        "stickies": rows,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
