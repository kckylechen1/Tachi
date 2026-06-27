use memory_core::{AuthorityLevel, MemoryEntry, ProjectionKind, TachiEventQuery, TachiEventRecord};
use serde_json::{json, Value};

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;

use super::storage::{get_projection_memory, read_events, upsert_projection_memory};
use super::{
    event_query_from_params, query_limit, target_from_event_params, ContinuityEventTarget,
};

mod entry;

use self::entry::{
    build_projection_entry, counter_i64, event_projections, projection_key, projection_memory_id,
};
pub(super) use self::entry::{projected_path_prefix, projection_filters, projection_kind_metadata};

pub(crate) fn project_continuity_events(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> Result<Value, String> {
    let target = target_from_event_params(server, params);
    let query = event_query_from_params(params);
    let filters = projection_filters(&params.projection_hints)?;
    project_continuity_events_inner(server, &target, query, filters, params.dry_run, false)
}

pub(crate) fn project_auto_continuity_events_for_target(
    server: &MemoryServer,
    target: ContinuityEventTarget,
    limit: usize,
) -> Result<Value, String> {
    let query = TachiEventQuery {
        limit: query_limit(limit),
        ..TachiEventQuery::default()
    };
    project_continuity_events_inner(server, &target, query, Vec::new(), false, true)
}

fn auto_projectable_event(event: &TachiEventRecord) -> bool {
    if event_projections(event).is_empty() {
        return false;
    }
    if matches!(
        event.authority,
        AuthorityLevel::Blocker | AuthorityLevel::ExecutionGate
    ) {
        return false;
    }

    let event_type = event.event_type.trim().to_ascii_lowercase();
    matches!(
        event.authority,
        AuthorityLevel::CollectOnly
            | AuthorityLevel::RawFact
            | AuthorityLevel::ReviewSignalOnly
            | AuthorityLevel::ToneAndReminderOnly
            | AuthorityLevel::Advisory
    ) || matches!(
        event_type.as_str(),
        "memory.saved"
            | "wiki.saved"
            | "session.captured"
            | "task.outcome"
            | "subagent.evaluated"
            | "session.outcome"
    )
}

fn project_continuity_events_inner(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    query: TachiEventQuery,
    filters: Vec<ProjectionKind>,
    dry_run: bool,
    auto_only: bool,
) -> Result<Value, String> {
    let events = read_events(server, target, &query)?;
    let mut projected = Vec::new();
    let mut skipped = Vec::new();
    let mut errors = Vec::new();
    let mut promotion_candidates = Vec::new();

    for event in events {
        if auto_only && !auto_projectable_event(&event) {
            skipped.push(json!({
                "event_id": event.id,
                "event_type": event.event_type,
                "reason": "not auto-projectable",
            }));
            continue;
        }
        let projections = event_projections(&event);
        if projections.is_empty() {
            skipped.push(json!({
                "event_id": event.id,
                "reason": "no projection hint or inferable event type",
            }));
            continue;
        }
        for projection in projections {
            if !filters.is_empty() && !filters.contains(&projection) {
                continue;
            }
            let key = projection_key(&event, projection);
            let memory_id = projection_memory_id(projection, &key);
            let existing = get_projection_memory(server, target, &memory_id)?;
            let (entry, already_projected) = build_projection_entry(existing, &event, projection);
            if !dry_run {
                if let Err(error) = upsert_projection_memory(server, target, &entry) {
                    errors.push(json!({
                        "event_id": event.id,
                        "projection": projection.as_str(),
                        "memory_id": entry.id,
                        "error": error,
                    }));
                    continue;
                }
            }
            if let Some(reason) = projection_promotion_reason(&entry) {
                if !promotion_candidates.iter().any(|candidate: &Value| {
                    candidate.get("memory_id").and_then(Value::as_str) == Some(entry.id.as_str())
                }) {
                    promotion_candidates.push(json!({
                        "memory_id": entry.id,
                        "projection": projection.as_str(),
                        "path": entry.path,
                        "summary": entry.summary,
                        "tier": entry.tier,
                        "reason": reason,
                        "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
                    }));
                }
            }
            projected.push(json!({
                "event_id": event.id,
                "event_type": event.event_type,
                "projection": projection.as_str(),
                "memory_id": entry.id,
                "path": entry.path,
                "summary": entry.summary,
                "tier": entry.tier,
                "already_projected": already_projected,
                "dry_run": dry_run,
            }));
        }
    }

    Ok(json!({
        "status": if errors.is_empty() { "completed" } else { "partial" },
        "dry_run": dry_run,
        "auto_only": auto_only,
        "projected_count": projected.len(),
        "skipped_count": skipped.len(),
        "promotion_candidate_count": promotion_candidates.len(),
        "promotion_candidates": promotion_candidates,
        "error_count": errors.len(),
        "projections": projected,
        "skipped": skipped,
        "errors": errors,
    }))
}

fn projection_promotion_reason(entry: &MemoryEntry) -> Option<&'static str> {
    let projection = projection_kind_metadata(entry)?;
    if !matches!(projection, "pattern" | "bonding" | "world_book") {
        return None;
    }
    let seen = counter_i64(&entry.metadata, "seen");
    let hit = counter_i64(&entry.metadata, "hit");
    if seen >= 3 && hit > 0 {
        return Some("hit_threshold");
    }
    if entry.tier == "pattern" {
        return Some("reviewed_pattern_tier");
    }
    None
}
