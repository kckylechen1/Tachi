use chrono::Utc;
use serde_json::json;

use crate::memory_search_ops::{scrub_secrets, scrub_think_tags};
use crate::MemoryServer;

use super::identity::resolve_caller_agent_id;
use super::memo::{sticky_to_memory_entry, StickyMemo};
use super::pending::list_or_claim_stickies;

const DEFAULT_TTL_DAYS: u32 = 7;
const MAX_TTL_DAYS: u32 = 30;

pub(crate) struct StickyLeaveInput {
    pub(crate) text: String,
    pub(crate) to: Option<String>,
    pub(crate) ttl_days: Option<u32>,
    pub(crate) agent_id: Option<String>,
}

pub(crate) async fn handle_sticky_leave(
    server: &MemoryServer,
    input: StickyLeaveInput,
) -> Result<String, String> {
    if input.text.trim().is_empty() {
        return Err("text is required and must be non-empty when action='sticky_leave'".into());
    }
    // One-shot stdio channels have no persistent agent_profile/env identity
    // (see identity.rs), so the fallback chain below bottoms out at
    // "unknown-agent" for them. Mirror `sticky_check`'s `agent_id` param
    // (identity::resolve_caller_agent_id already sanitizes it — trim +
    // reject empty — before falling back through the same server-side chain
    // used when the param is absent) so a caller that *does* know its own
    // seat name can stamp it explicitly.
    //
    // #1016: a non-empty `params.agent_id` is the caller SELF-REPORTING an
    // identity the server never verifies (advisory-only trust, owner-ratified
    // same_host semantics) — distinct from every other link in the same
    // resolution chain (`agent_profile`, `TACHI_AGENT_SEAT`), which the
    // server resolved on its own. Compute that distinction independently of
    // `resolve_caller_agent_id`'s output so the from_agent resolution logic
    // itself is untouched — only which branch fired is now recorded.
    let is_caller_asserted = input
        .agent_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let identity_assurance = if is_caller_asserted {
        "caller_asserted"
    } else {
        "session"
    }
    .to_string();
    let from_agent = resolve_caller_agent_id(server, input.agent_id.as_deref())
        .unwrap_or_else(|| "unknown-agent".to_string());
    // Round-5 CONCERN (codex review of #1003): unbounded ttl_days let a caller
    // pass u32::MAX and get a sticky that never expires. Clamp to a sane
    // window (1-30 days) — the archive TTL is meant to bound unread-note
    // lifetime, not opt out of it.
    let ttl_days = input
        .ttl_days
        .unwrap_or(DEFAULT_TTL_DAYS)
        .clamp(1, MAX_TTL_DAYS);

    // CP4 (security): scrub before persisting — sticky bodies are stored
    // verbatim in the global DB and rendered verbatim into the leader
    // briefing markdown, so a bearer token / API key / AWS key left in a
    // sticky body must never round-trip raw. Mirrors
    // memory_search_ops::save_memory::handler::handle_save_memory's
    // scrub_think_tags -> scrub_secrets order.
    let scrubbed_text = scrub_think_tags(&input.text);
    let (safe_text, _secret_redactions) = scrub_secrets(&scrubbed_text);

    let memo = StickyMemo {
        id: uuid::Uuid::new_v4().to_string(),
        from_agent: from_agent.clone(),
        to: input.to.clone(),
        text: safe_text,
        created_at: Utc::now().to_rfc3339(),
        ttl_days,
        identity_assurance: identity_assurance.clone(),
    };

    let entry = sticky_to_memory_entry(server, &memo);
    server.with_global_store(|store| store.upsert(&entry).map_err(|e| format!("{e}")))?;

    serde_json::to_string(&json!({
        "status": "sticky_left",
        "sticky_id": memo.id,
        "from_agent": from_agent,
        "to": memo.to,
        "ttl_days": ttl_days,
        "identity_assurance": identity_assurance,
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
    // CP2: resolve identity server-side (params.agent_id -> agent_profile ->
    // TACHI_AGENT_SEAT env -> leader), same chain sticky_leave now shares
    // for its own agent_id override — see identity::resolve_caller_agent_id.
    let agent_id = resolve_caller_agent_id(server, input.agent_id.as_deref());
    let limit = input.limit.unwrap_or(10);
    let rows = list_or_claim_stickies(server, agent_id.as_deref(), input.include_read, limit)?;

    serde_json::to_string(&json!({
        "count": rows.len(),
        "include_read": input.include_read,
        "stickies": rows,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
