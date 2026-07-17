use crate::server_state::HandoffMemo;
use memcore::MemoryEntry;

// #1099: `memo_matches_agent` (used only by the retired `handoff_check`
// action) and `memo_to_memory_entry` (used only by the retired
// `handoff_leave` action, which was the sole writer of new `handoff:<id>`
// entries) are gone along with those routes. `memo_from_entry` survives —
// `promote_issue` still reads pre-existing handoff entries through it.

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
