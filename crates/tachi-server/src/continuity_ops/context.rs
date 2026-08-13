use std::collections::HashSet;

use memcore::{AuthorityLevel, EffectScope, MemoryEntry, ProjectionKind, TachiEventRecord};
use serde_json::{json, Value};

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;
use memory_server_runtime::{query_limit, trim_opt};

use super::feedback::pattern_ref_json;
use super::projection::{projected_path_prefix, projection_filters, projection_kind_metadata};
use super::read_models::{
    a2a_context_bundle, bonding_context_json, host_lifecycle_contract, timeline_context_json,
};
use super::storage::{continuity_metrics, list_projection_memories, read_events};
use super::{event_query_from_params, target_from_event_params, ContinuityEventTarget};

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
    let haystack = [
        entry.id.as_str(),
        entry.path.as_str(),
        entry.summary.as_str(),
        entry.text.as_str(),
        entry.topic.as_str(),
        projection_key,
    ]
    .iter()
    .map(|value| value.to_ascii_lowercase())
    .collect::<Vec<_>>()
    .join("\n");
    if haystack.contains(&query) {
        return true;
    }
    query
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == ';' || ch == ':')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 3)
        .any(|token| haystack.contains(token))
}

fn pattern_context_json(entry: &MemoryEntry) -> Value {
    let pattern_ref = pattern_ref_json(entry);
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
        "content": entry.text,
        "pattern_ref": pattern_ref,
        "projection_kind": projection_kind_metadata(entry).unwrap_or("pattern"),
        "projection_key": entry.metadata.get("projection_key").cloned().unwrap_or(Value::Null),
        "authority": entry.metadata.get("authority").cloned().unwrap_or(Value::Null),
        "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
        "source_event_id": entry.metadata.get("source_event_id").cloned().unwrap_or(Value::Null),
        "projected_event_ids": entry.metadata.get("projected_event_ids").cloned().unwrap_or_else(|| json!([])),
    })
}

fn legacy_feedback_projection(pattern: &Value) -> ProjectionKind {
    match pattern
        .get("projection_kind")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        Some("bonding") => ProjectionKind::Bonding,
        _ => ProjectionKind::Pattern,
    }
}

fn normalize_legacy_preview_report(report: &mut Value, prior_event_ids: &HashSet<String>) {
    match report {
        Value::Object(object) => {
            if object.contains_key("dry_run") {
                object.insert("dry_run".to_string(), json!(false));
            }
            if object
                .get("event_id")
                .and_then(Value::as_str)
                .is_some_and(|event_id| prior_event_ids.contains(event_id))
            {
                object.insert("already_projected".to_string(), json!(true));
            }
            for value in object.values_mut() {
                normalize_legacy_preview_report(value, prior_event_ids);
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize_legacy_preview_report(value, prior_event_ids);
            }
        }
        _ => {}
    }
}

fn legacy_context_feedback_receipt(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    query: Option<&str>,
    patterns: &[Value],
) -> Value {
    let mut seen_keys = HashSet::new();
    let mut prior_events = Vec::<TachiEventRecord>::new();
    let mut saved = Vec::new();
    let mut errors = Vec::new();

    for pattern in patterns {
        let Some(projection_key) = pattern
            .get("projection_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if !seen_keys.insert(projection_key.to_string()) {
            continue;
        }
        let pattern_id = pattern
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(projection_key);
        let projection = legacy_feedback_projection(pattern);
        let projection_name = projection.as_str();
        let event_type = format!("{projection_name}.seen");
        let event_id = super::stable_event_payload_id(&[
            "context.feedback.preview",
            pattern_id,
            projection_key,
            query.unwrap_or_default(),
        ]);
        let event = TachiEventRecord {
            id: event_id.clone(),
            source_repo: "tachi".to_string(),
            adapter: "tachi_event.context".to_string(),
            project: target.project_label(None),
            domain: "pattern_memory".to_string(),
            session_id: query.unwrap_or_default().to_string(),
            actor: "tachi_memory".to_string(),
            event_type: event_type.clone(),
            authority: AuthorityLevel::CollectOnly,
            effects: vec![EffectScope::Recall],
            projection_hints: vec![projection],
            payload: json!({
                "projection_key": projection_key,
                "pattern_id": pattern_id,
                "outcome": "seen",
                "query": query.unwrap_or_default(),
                "note": "",
                "metadata": {},
            }),
            provenance: json!({
                "source": "pattern_feedback",
                "note": "pattern feedback updates continuity projection counters via the event ledger",
            }),
            created_at: pattern
                .get("counters")
                .and_then(|counters| counters.get("last_seen"))
                .and_then(Value::as_str)
                .unwrap_or("1970-01-01T00:00:00Z")
                .to_string(),
        };
        let mut preview_events = vec![event.clone()];
        preview_events.extend(prior_events.iter().rev().cloned());
        let prior_event_ids = prior_events
            .iter()
            .map(|event| event.id.clone())
            .collect::<HashSet<_>>();
        match super::projection::preview_auto_projection_with_events(
            server,
            target,
            50,
            preview_events,
        ) {
            Ok(mut projection_report) => {
                normalize_legacy_preview_report(&mut projection_report, &prior_event_ids);
                saved.push(json!({
                    "status": "saved",
                    "event_id": event_id,
                    "event_type": event_type,
                    "pattern_id": pattern_id,
                    "projection_key": projection_key,
                    "projection": projection_name,
                    "outcome": "seen",
                    "projection_report": projection_report,
                }));
                prior_events.push(event);
            }
            Err(error) => errors.push(json!({
                "pattern_id": pattern_id,
                "projection_key": projection_key,
                "error": error,
            })),
        }
    }

    json!({
        "status": if errors.is_empty() { "saved" } else { "partial" },
        "saved_count": saved.len(),
        "error_count": errors.len(),
        "events": saved,
        "errors": errors,
    })
}

fn is_projection(entry: &MemoryEntry, projection: &str, prefix: &str) -> bool {
    entry.path.starts_with(prefix)
        || entry
            .metadata
            .get("projection_kind")
            .and_then(Value::as_str)
            == Some(projection)
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
    build_continuity_context_inner(server, params, true)
}

pub(crate) fn build_a2a_context(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> Result<Value, String> {
    let context = build_continuity_context_inner(server, params, false)?;
    Ok(json!({
        "status": "completed",
        "action": "a2a",
        "a2a": context.get("a2a").cloned().unwrap_or(Value::Null),
        "host_lifecycle": context.get("host_lifecycle").cloned().unwrap_or(Value::Null),
        "guardrails": context.get("guardrails").cloned().unwrap_or(Value::Null),
    }))
}

fn build_continuity_context_inner(
    server: &MemoryServer,
    params: &TachiEventParams,
    record_seen: bool,
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
            // tachi#1561 (L4): the `memories` array below is emitted verbatim
            // — the `is_projection` filters further down only build the extra
            // typed arrays, they never gate the dump. `params.path_prefix` is
            // caller-controlled, so `tachi_event(action='context',
            // path_prefix='/')` walked the whole store into the response.
            // Filter with the same predicate search/list use, scoped to the
            // prefix this row was listed under: for the default projection
            // prefixes `path_prefix_opts_into_continuity_projection` opts the
            // projections back in (they are exactly what this surface exists
            // to return), while wiki `_log` / `wiki-rem:` / recall-cache /
            // anchor / kanban / handoff rows stay out under any prefix.
            if memcore::is_namespace_search_noise(&entry, Some(prefix.as_str())) {
                continue;
            }
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
        .filter(|entry| is_projection(entry, "world_book", "/lorebook"))
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
    let pattern_refs = patterns
        .iter()
        .filter_map(|pattern| pattern.get("pattern_ref").cloned())
        .collect::<Vec<_>>();
    let affect = memories
        .iter()
        .filter(|entry| is_projection(entry, "affect", "/user/affect"))
        .map(|entry| {
            json!({
                "id": entry.id,
                "path": entry.path,
                "summary": entry.summary,
                "state": entry.summary,
                "affect": entry.metadata.get("affect").cloned().unwrap_or_else(|| json!({})),
                "guardrails": entry.metadata.get("guardrails").cloned().unwrap_or_else(|| json!({})),
                "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect::<Vec<_>>();
    let bonding = memories
        .iter()
        .filter(|entry| is_projection(entry, "bonding", "/user/patterns/bonding"))
        .map(bonding_context_json)
        .collect::<Vec<_>>();
    let timeline = memories
        .iter()
        .filter(|entry| is_projection(entry, "timeline", "/timeline"))
        .map(timeline_context_json)
        .collect::<Vec<_>>();
    let a2a = a2a_context_bundle(&pattern_refs, &bonding, &timeline, &events);
    let feedback = if record_seen {
        legacy_context_feedback_receipt(
            server,
            &target,
            query
                .event_type
                .as_deref()
                .or(query.session_id.as_deref())
                .or(query.domain.as_deref()),
            &patterns,
        )
    } else {
        json!({
            "status": "skipped",
            "reason": "read_only_bundle",
        })
    };
    let host_lifecycle = host_lifecycle_contract();

    Ok(json!({
        "status": "completed",
        "projection_hints": filters.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        "memory_count": memories.len(),
        "event_count": events.len(),
        "memories": memories,
        "events": events,
        "patterns": patterns,
        "pattern_refs": pattern_refs,
        "lorebook": lorebook,
        "affect": affect,
        "bonding": bonding,
        "timeline": timeline,
        "a2a": a2a,
        "host_lifecycle": host_lifecycle,
        "feedback": feedback,
        "metrics": metrics,
        "guardrails": {
            "a2a": "share evidence and open questions, not conclusions",
            "affect": "tone_and_reminder_only; never scoring, execution, or fact mutation",
            "projection": "event projection is explicit/idempotent; append-only events remain the source ledger"
        }
    }))
}
