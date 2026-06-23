use crate::server_state::{DbScope, HandoffMemo, MemoryServer};
use memory_core::MemoryEntry;

pub(super) fn memo_matches_agent(memo: &HandoffMemo, agent_id: Option<&str>) -> bool {
    if memo.acknowledged {
        return false;
    }

    match (agent_id, memo.target_agent.as_deref()) {
        (_, None) => true,
        (Some(my_id), Some(target)) => my_id == target,
        (None, Some(_)) => true,
    }
}

pub(super) fn memo_to_memory_entry(server: &MemoryServer, memo: &HandoffMemo) -> MemoryEntry {
    let memo_id = memo.id.clone();
    let mut metadata = crate::provenance::inject_provenance(
        server,
        serde_json::json!({
            "handoff_memo_id": memo_id,
            "handoff": memo,
            "status": "pending",
        }),
        "handoff_leave",
        "handoff_memo",
        Some("general"),
        DbScope::Global,
        serde_json::json!({
            "from_agent": memo.from_agent.clone(),
            "target_agent": memo.target_agent.clone(),
            "next_steps_count": memo.next_steps.len(),
        }),
    );
    // Handoff lives in the global DB but uses a non-/global path prefix; opt
    // in to cross-project routing so path-routing validation lets it through.
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            "allow_cross_project".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    let routed_path =
        memory_core::path_router::standardize_handoff_path(memo.target_agent.as_deref());

    MemoryEntry {
        id: format!("handoff:{}", memo_id),
        text: format!(
            "[Handoff from {}] {}\n\nNext steps:\n{}",
            memo.from_agent,
            memo.summary,
            memo.next_steps
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        category: "handoff".to_string(),
        importance: 0.75,
        summary: format!("Handoff from {}", memo.from_agent),
        path: routed_path,
        timestamp: memo.created_at.clone(),
        valid_from: String::new(),
        valid_until: None,
        topic: "agent-handoff".to_string(),
        keywords: vec!["handoff".to_string(), memo.from_agent.clone()],
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
        retention_policy: Some(memory_core::RetentionPolicy::Pinned.as_str().to_string()),
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

pub(super) fn memo_from_entry(entry: &MemoryEntry) -> HandoffMemo {
    if let Some(memo) = entry
        .metadata
        .get("handoff")
        .and_then(|value| serde_json::from_value::<HandoffMemo>(value.clone()).ok())
    {
        let acknowledged = handoff_acknowledged(entry, &memo);
        return HandoffMemo {
            acknowledged,
            ..memo
        };
    }

    let metadata = entry.metadata.as_object();
    let from_agent = metadata
        .and_then(|m| m.get("provenance"))
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("from_agent"))
        .and_then(|v| v.as_str())
        .or_else(|| entry.entities.first().map(String::as_str))
        .unwrap_or("unknown-agent")
        .to_string();
    let target_agent = metadata
        .and_then(|m| m.get("provenance"))
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("target_agent"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let memo_id = metadata
        .and_then(|m| m.get("handoff_memo_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| entry.id.strip_prefix("handoff:").map(str::to_string))
        .unwrap_or_else(|| entry.id.clone());

    HandoffMemo {
        id: memo_id,
        from_agent,
        target_agent,
        summary: legacy_summary_from_entry(entry),
        next_steps: legacy_next_steps_from_entry(entry),
        context: None,
        created_at: entry.timestamp.clone(),
        acknowledged: handoff_acknowledged(entry, &empty_handoff_memo()),
    }
}

fn legacy_summary_from_entry(entry: &MemoryEntry) -> String {
    entry
        .text
        .strip_prefix("[Handoff from ")
        .and_then(|rest| rest.split_once("] "))
        .map(|(_, summary_and_steps)| {
            summary_and_steps
                .split_once("\n\nNext steps:")
                .map(|(summary, _)| summary)
                .unwrap_or(summary_and_steps)
                .trim()
                .to_string()
        })
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| entry.summary.clone())
}

fn legacy_next_steps_from_entry(entry: &MemoryEntry) -> Vec<String> {
    let Some((_, raw_steps)) = entry.text.split_once("\n\nNext steps:") else {
        return Vec::new();
    };

    raw_steps
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let Some((prefix, step)) = line.split_once(". ") else {
                return line.to_string();
            };
            if prefix.chars().all(|c| c.is_ascii_digit()) {
                step.trim().to_string()
            } else {
                line.to_string()
            }
        })
        .collect()
}

fn empty_handoff_memo() -> HandoffMemo {
    HandoffMemo {
        id: String::new(),
        from_agent: String::new(),
        target_agent: None,
        summary: String::new(),
        next_steps: Vec::new(),
        context: None,
        created_at: String::new(),
        acknowledged: false,
    }
}

fn handoff_acknowledged(entry: &MemoryEntry, memo: &HandoffMemo) -> bool {
    memo.acknowledged
        || entry.archived
        || entry
            .metadata
            .get("status")
            .and_then(|value| value.as_str())
            .is_some_and(|status| matches!(status, "acknowledged" | "promoted"))
        || entry
            .metadata
            .get("acknowledged")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
}
