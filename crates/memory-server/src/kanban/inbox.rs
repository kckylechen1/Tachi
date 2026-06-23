use super::normalize::*;
use super::types::CheckInboxParams;
use super::*;

pub(super) fn card_matches_inbox(
    entry: &MemoryEntry,
    params: &CheckInboxParams,
    agent_id: &str,
) -> bool {
    if entry.category != KANBAN_CATEGORY || entry.archived {
        return false;
    }
    let to_agent = match card_to_agent(entry) {
        Some(v) => v,
        None => return false,
    };
    if to_agent != agent_id && !(params.include_broadcast && to_agent == "*") {
        return false;
    }
    if let Some(filter) = params
        .status_filter
        .as_ref()
        .and_then(|v| normalize_card_status(v))
    {
        if card_status(entry).as_deref() != Some(filter.as_str()) {
            return false;
        }
    }
    if let Some(since) = params.since.as_ref() {
        if entry.timestamp < *since {
            return false;
        }
    }
    if let Some(ref ws_filter) = params.workspace_id {
        let card_ws = card_metadata_str(entry, "workspace_id")
            .or_else(|| card_metadata_str(entry, "project_id"))
            .unwrap_or_default();
        if card_ws != *ws_filter {
            return false;
        }
    }
    if let Some(ref conv_filter) = params.conversation_id {
        let card_conv = card_metadata_str(entry, "conversation_id").unwrap_or_default();
        if card_conv != *conv_filter {
            return false;
        }
    }
    true
}
