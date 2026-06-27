use memory_core::{MemoryEntry, ProjectionKind};
use serde_json::{json, Value};

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;

use super::projection::{projected_path_prefix, projection_filters, projection_kind_metadata};
use super::storage::{continuity_metrics, list_projection_memories, read_events};
use super::{
    event_query_from_params, query_limit, target_from_event_params, trim_opt, ContinuityEventTarget,
};

fn is_active_pattern_projection(entry: &MemoryEntry) -> bool {
    if !entry.path.starts_with("/user/patterns") {
        return false;
    }
    matches!(
        projection_kind_metadata(entry),
        Some("pattern") | Some("bonding") | None
    )
}

fn pattern_matches_query(entry: &MemoryEntry, query: Option<&str>) -> bool {
    let Some(query) = query.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let query = query.to_ascii_lowercase();
    let projection_key = entry
        .metadata
        .get("projection_key")
        .and_then(Value::as_str)
        .unwrap_or_default();
    [
        entry.id.as_str(),
        entry.path.as_str(),
        entry.summary.as_str(),
        entry.text.as_str(),
        entry.topic.as_str(),
        projection_key,
    ]
    .iter()
    .any(|value| value.to_ascii_lowercase().contains(&query))
}

fn pattern_context_json(entry: &MemoryEntry) -> Value {
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
        "content": entry.text,
        "projection_kind": projection_kind_metadata(entry).unwrap_or("pattern"),
        "projection_key": entry.metadata.get("projection_key").cloned().unwrap_or(Value::Null),
        "authority": entry.metadata.get("authority").cloned().unwrap_or(Value::Null),
        "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
        "source_event_id": entry.metadata.get("source_event_id").cloned().unwrap_or(Value::Null),
        "projected_event_ids": entry.metadata.get("projected_event_ids").cloned().unwrap_or_else(|| json!([])),
    })
}

pub(crate) fn list_active_patterns(
    server: &MemoryServer,
    project: Option<&str>,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryEntry>, String> {
    let target = ContinuityEventTarget::from_default_write(server, project);
    let limit = query_limit(limit);
    let scan_limit = limit.saturating_mul(4).max(50).min(500);
    let mut patterns = list_projection_memories(server, &target, "/user/patterns", scan_limit)?
        .into_iter()
        .filter(is_active_pattern_projection)
        .filter(|entry| pattern_matches_query(entry, query))
        .collect::<Vec<_>>();
    patterns.truncate(limit);
    Ok(patterns)
}

fn default_context_projections(filters: Vec<ProjectionKind>) -> Vec<ProjectionKind> {
    if filters.is_empty() {
        vec![
            ProjectionKind::Pattern,
            ProjectionKind::Bonding,
            ProjectionKind::Affect,
            ProjectionKind::WorldBook,
            ProjectionKind::Timeline,
            ProjectionKind::ProjectCycle,
            ProjectionKind::DomainProfile,
            ProjectionKind::Outcome,
            ProjectionKind::EvidenceGate,
        ]
    } else {
        filters
    }
}

pub(crate) fn build_continuity_context(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> Result<Value, String> {
    let target = target_from_event_params(server, params);
    let filters = default_context_projections(projection_filters(&params.projection_hints)?);
    let limit = query_limit(params.limit);
    let path_prefix_override = trim_opt(&params.path_prefix);
    let mut seen_ids = Vec::<String>::new();
    let mut memories = Vec::new();

    let prefixes = if let Some(prefix) = path_prefix_override {
        vec![prefix]
    } else {
        filters
            .iter()
            .map(|projection| projected_path_prefix(*projection).to_string())
            .collect()
    };

    for prefix in prefixes {
        for entry in list_projection_memories(server, &target, &prefix, limit)? {
            if seen_ids.iter().any(|id| id == &entry.id) {
                continue;
            }
            seen_ids.push(entry.id.clone());
            memories.push(entry);
            if memories.len() >= limit {
                break;
            }
        }
        if memories.len() >= limit {
            break;
        }
    }

    let query = event_query_from_params(params);
    let events = read_events(server, &target, &query)?;
    let metrics = continuity_metrics(server, &target, limit)?;

    let lorebook = memories
        .iter()
        .filter(|entry| {
            entry
                .metadata
                .get("projection_kind")
                .and_then(Value::as_str)
                == Some("world_book")
        })
        .map(|entry| {
            json!({
                "id": entry.id,
                "path": entry.path,
                "summary": entry.summary,
                "content": entry.text,
                "lorebook": entry.metadata.get("lorebook").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect::<Vec<_>>();
    let patterns = memories
        .iter()
        .filter(|entry| is_active_pattern_projection(entry))
        .map(pattern_context_json)
        .collect::<Vec<_>>();
    let affect = memories
        .iter()
        .filter(|entry| {
            entry
                .metadata
                .get("projection_kind")
                .and_then(Value::as_str)
                == Some("affect")
        })
        .map(|entry| {
            json!({
                "id": entry.id,
                "path": entry.path,
                "summary": entry.summary,
                "state": entry.summary,
                "guardrails": entry.metadata.get("guardrails").cloned().unwrap_or_else(|| json!({})),
                "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "status": "completed",
        "projection_hints": filters.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        "memory_count": memories.len(),
        "event_count": events.len(),
        "memories": memories,
        "events": events,
        "patterns": patterns,
        "lorebook": lorebook,
        "affect": affect,
        "metrics": metrics,
        "guardrails": {
            "a2a": "share evidence and open questions, not conclusions",
            "affect": "tone_and_reminder_only; never scoring, execution, or fact mutation",
            "projection": "event projection is explicit/idempotent; append-only events remain the source ledger"
        }
    }))
}
