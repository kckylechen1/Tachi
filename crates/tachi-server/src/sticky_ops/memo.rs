use crate::server_state::{DbScope, MemoryServer};
use memcore::MemoryEntry;
use serde::{Deserialize, Serialize};

/// A single sticky note: an ephemeral read-once agent-to-agent memo.
///
/// Addressing (frozen 2026-07-11): `to` absent means addressed to the
/// leader/main session ONLY — worker seats never consume unaddressed
/// stickies. `to: Some(seat)` is visible only to that seat name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StickyMemo {
    pub(crate) id: String,
    pub(crate) from_agent: String,
    pub(crate) to: Option<String>,
    pub(crate) text: String,
    pub(crate) created_at: String,
    pub(crate) ttl_days: u32,
}

/// Whether `memo` is visible to a caller identified by `agent_id`.
///
/// `agent_id: None` is treated as the leader (frozen semantics #3/#4): it
/// sees only broadcast stickies (`to` absent). A named seat sees only
/// stickies explicitly addressed to that seat name — never broadcast ones.
pub(super) fn sticky_visible_to(memo: &StickyMemo, agent_id: Option<&str>) -> bool {
    match (agent_id, memo.to.as_deref()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(_), None) => false,
        (Some(caller), Some(target)) => caller == target,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn sticky_to_memory_entry(server: &MemoryServer, memo: &StickyMemo) -> MemoryEntry {
    let sticky_id = memo.id.clone();
    let mut metadata = crate::provenance::inject_provenance(
        server,
        serde_json::json!({
            "sticky_id": sticky_id,
            "sticky": memo,
            "status": "unread",
        }),
        "sticky_leave",
        "sticky_memo",
        Some("general"),
        DbScope::Global,
        serde_json::json!({
            "from_agent": memo.from_agent.clone(),
            "to": memo.to.clone(),
        }),
    );
    // Stickies live in the global DB but use a non-/global path prefix,
    // matching handoff's own cross-project routing opt-in (see
    // handoff_ops::memo::memo_to_memory_entry for the identical pattern).
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            "allow_cross_project".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    let routed_path = memcore::path_router::standardize_sticky_path(memo.to.as_deref());

    MemoryEntry {
        id: format!("sticky:{}", sticky_id),
        text: memo.text.clone(),
        category: "sticky".to_string(),
        importance: 0.6,
        summary: format!("Sticky from {}", memo.from_agent),
        path: routed_path,
        timestamp: memo.created_at.clone(),
        valid_from: String::new(),
        valid_until: None,
        topic: "agent-sticky".to_string(),
        keywords: vec!["sticky".to_string(), memo.from_agent.clone()],
        persons: vec![],
        entities: vec![memo.from_agent.clone()],
        location: String::new(),
        source: "extraction".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        metadata,
        retention_policy: Some(memcore::RetentionPolicy::Ephemeral.as_str().to_string()),
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

pub(super) fn sticky_from_entry(entry: &MemoryEntry) -> Option<StickyMemo> {
    entry
        .metadata
        .get("sticky")
        .and_then(|value| serde_json::from_value::<StickyMemo>(value.clone()).ok())
}

/// TTL check (frozen semantics #6): unread past `ttl_days` -> treated as
/// expired. Evaluated inline at read time (not just in the batch GC sweep)
/// so expiry is correct even if the periodic GC job hasn't run yet.
pub(super) fn sticky_ttl_expired(memo: &StickyMemo, now: chrono::DateTime<chrono::Utc>) -> bool {
    let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(&memo.created_at) else {
        return false;
    };
    let ttl_days = i64::from(memo.ttl_days.max(1));
    now >= created_at.with_timezone(&chrono::Utc) + chrono::Duration::days(ttl_days)
}

/// Read-side "unread" test: a sticky is unread iff its DB row is neither
/// archived nor status=claimed/expired. The atomic claim itself is enforced
/// by the `hard_state` CAS gate (see `claim.rs`), NOT by this metadata flag —
/// this only mirrors the outcome back onto the row for display/TTL sweep.
pub(super) fn sticky_row_is_unread(entry: &MemoryEntry) -> bool {
    if entry.archived {
        return false;
    }
    !entry
        .metadata
        .get("status")
        .and_then(|value| value.as_str())
        .is_some_and(|status| matches!(status, "claimed" | "expired"))
}
