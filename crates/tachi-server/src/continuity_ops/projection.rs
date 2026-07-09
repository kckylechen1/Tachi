use memcore::{
    AuthorityLevel, MemoryEdge, MemoryEntry, ProjectionKind, TachiEventQuery, TachiEventRecord,
};
use serde_json::{json, Value};

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;
use tachi_runtime::query_limit;

use super::storage::{
    add_memory_edge, get_projection_memory, read_events, upsert_projection_memory,
};
use super::{event_query_from_params, target_from_event_params, ContinuityEventTarget};

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
            let graph_edges = if projection == ProjectionKind::Timeline {
                persist_timeline_graph_edges(server, target, &entry, &event, dry_run)
            } else {
                json!({
                    "saved_count": 0,
                    "skipped_count": 0,
                    "edges": [],
                    "skipped": [],
                })
            };
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
                        "review_artifacts": maturity_review_artifacts(&entry, reason),
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
                "graph_edges": graph_edges,
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

fn nested_event_payload(event: &TachiEventRecord) -> &Value {
    event.payload.get("candidate").unwrap_or(&event.payload)
}

fn edge_endpoint(raw: &Value, keys: &[&str], projection_id: &str) -> Option<String> {
    keys.iter()
        .find_map(|key| raw.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if matches!(value, "self" | "$self" | "projection" | "$projection") {
                projection_id.to_string()
            } else {
                value.to_string()
            }
        })
}

fn persist_timeline_graph_edges(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    entry: &MemoryEntry,
    event: &TachiEventRecord,
    dry_run: bool,
) -> Value {
    let payload = nested_event_payload(event);
    let Some(edges) = payload.get("causal_edges").and_then(Value::as_array) else {
        return json!({
            "saved_count": 0,
            "skipped_count": 0,
            "edges": [],
            "skipped": [],
        });
    };
    let mut saved = Vec::new();
    let mut skipped = Vec::new();
    for raw in edges {
        let Some(source_id) =
            edge_endpoint(raw, &["source_id", "from_memory_id", "from_id"], &entry.id)
        else {
            skipped.push(json!({"edge": raw, "reason": "missing source_id"}));
            continue;
        };
        let Some(target_id) =
            edge_endpoint(raw, &["target_id", "to_memory_id", "to_id"], &entry.id)
        else {
            skipped.push(json!({"edge": raw, "reason": "missing target_id"}));
            continue;
        };
        let relation = raw
            .get("relation")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("causes")
            .to_string();
        let source_exists = get_projection_memory(server, target, &source_id)
            .ok()
            .flatten()
            .is_some();
        let target_exists = get_projection_memory(server, target, &target_id)
            .ok()
            .flatten()
            .is_some();
        if !source_exists || !target_exists {
            skipped.push(json!({
                "edge": raw,
                "source_id": source_id,
                "target_id": target_id,
                "reason": "endpoint memory missing",
            }));
            continue;
        }
        let edge = MemoryEdge {
            source_id: source_id.clone(),
            target_id: target_id.clone(),
            relation: relation.clone(),
            weight: raw.get("weight").and_then(Value::as_f64).unwrap_or(1.0),
            metadata: json!({
                "source_event_id": event.id,
                "timeline_projection_id": entry.id,
                "raw_edge": raw,
            }),
            created_at: event.created_at.clone(),
            valid_from: raw
                .get("valid_from")
                .and_then(Value::as_str)
                .unwrap_or(event.created_at.as_str())
                .to_string(),
            valid_to: raw
                .get("valid_to")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        if !dry_run {
            if let Err(error) = add_memory_edge(server, target, &edge) {
                skipped.push(json!({
                    "edge": raw,
                    "source_id": source_id,
                    "target_id": target_id,
                    "relation": relation,
                    "reason": error,
                }));
                continue;
            }
        }
        saved.push(json!({
            "source_id": source_id,
            "target_id": target_id,
            "relation": relation,
            "dry_run": dry_run,
        }));
    }
    json!({
        "saved_count": saved.len(),
        "skipped_count": skipped.len(),
        "edges": saved,
        "skipped": skipped,
    })
}

fn projection_promotion_reason(entry: &MemoryEntry) -> Option<&'static str> {
    let projection = projection_kind_metadata(entry)?;
    if !matches!(projection, "pattern" | "bonding" | "world_book") {
        return None;
    }
    let seen = counter_i64(&entry.metadata, "seen");
    let hit = counter_i64(&entry.metadata, "hit");
    let miss = counter_i64(&entry.metadata, "miss");
    if seen >= 3 && hit > 0 && hit > miss {
        return Some("hit_threshold");
    }
    if entry.tier == "pattern" {
        return Some("reviewed_pattern_tier");
    }
    None
}

fn promotion_slug(entry: &MemoryEntry) -> String {
    let raw = entry
        .metadata
        .get("projection_key")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(entry.id.as_str());
    let mut out = String::new();
    let mut previous_sep = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            previous_sep = false;
        } else if !previous_sep && !out.is_empty() {
            out.push('-');
            previous_sep = true;
        }
        if out.len() >= 72 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        entry.id.clone()
    } else {
        out
    }
}

fn non_empty_array_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Vec<Value>> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_array().filter(|items| !items.is_empty())
}

fn bool_at(value: &Value, path: &[&str]) -> bool {
    let mut cursor = value;
    for key in path {
        let Some(next) = cursor.get(*key) else {
            return false;
        };
        cursor = next;
    }
    cursor.as_bool().unwrap_or(false)
}

fn promotion_gate(entry: &MemoryEntry) -> Value {
    let external_validation =
        non_empty_array_at(&entry.metadata, &["timeline", "external_validations"]).is_some()
            || non_empty_array_at(&entry.metadata, &["external_validations"]).is_some()
            || bool_at(&entry.metadata, &["promotion_gate", "external_validation"]);
    let cold_seat_review = bool_at(&entry.metadata, &["promotion_gate", "cold_seat_review"])
        || bool_at(&entry.metadata, &["cold_seat", "reviewed"])
        || non_empty_array_at(&entry.metadata, &["cold_seat", "checks"]).is_some();
    let final_ready = external_validation && cold_seat_review;
    let mut missing = Vec::new();
    if !external_validation {
        missing.push("external_validation");
    }
    if !cold_seat_review {
        missing.push("cold_seat_review");
    }
    json!({
        "final_ready": final_ready,
        "review_required": !final_ready,
        "external_validation": external_validation,
        "cold_seat_review": cold_seat_review,
        "missing": missing,
        "rule": "final promotion requires external validation and cold-seat review",
    })
}

fn maturity_review_artifacts(entry: &MemoryEntry, reason: &str) -> Value {
    let slug = promotion_slug(entry);
    let pattern_ref = crate::continuity_ops::pattern_ref_json(entry);
    let gate = promotion_gate(entry);
    json!({
        "review_required": true,
        "auto_promote": false,
        "gate": gate,
        "wiki_draft": {
            "tool": "tachi_wiki_write",
            "path": format!("/wiki/drafts/patterns/{slug}"),
            "title": format!("Pattern Review: {}", entry.summary),
            "include_patterns": true,
            "pattern_query": entry.metadata.get("projection_key").and_then(Value::as_str).unwrap_or(entry.id.as_str()),
            "review_status": "pending",
            "reason": reason,
            "gate": gate,
            "pattern_ref": pattern_ref,
        },
        "skill_candidate": {
            "tool": "tachi_skill",
            "action": "from_pattern",
            "args": {
                "pattern_ref": entry.id,
                "skill_id": format!("skill:pattern-{slug}"),
                "name": format!("Pattern: {}", entry.summary),
            },
            "enabled": false,
            "review_status": "pending",
            "gate": gate,
        },
        "agent_profile_proposal": {
            "tool": "project_agent_profile",
            "write": false,
            "review_status": "pending",
            "input": {
                "pattern_ref": pattern_ref,
                "reason": reason,
            },
        },
    })
}
