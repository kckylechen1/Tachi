use std::collections::HashSet;

use memory_core::{AuthorityLevel, EffectScope, MemoryEntry, ProjectionKind, TachiEventRecord};
use serde_json::{json, Value};

use crate::{DbScope, MemoryServer};

use super::storage::write_event;
use super::{now_rfc3339, stable_event_payload_id, ContinuityEventTarget};

pub(crate) fn emit_session_captured_event(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    conversation_id: &str,
    turn_id: &str,
    agent_id: &str,
    path_prefix: &str,
    captured_memory_ids: &[String],
    message_count: usize,
    project: Option<&str>,
) -> Value {
    let event = TachiEventRecord {
        id: stable_event_payload_id(&[
            "session.captured",
            conversation_id,
            turn_id,
            agent_id,
            &captured_memory_ids.join(","),
        ]),
        source_repo: "tachi".to_string(),
        adapter: "capture_session".to_string(),
        project: target.project_label(project),
        domain: "session".to_string(),
        session_id: conversation_id.to_string(),
        actor: agent_id.to_string(),
        event_type: "session.captured".to_string(),
        authority: AuthorityLevel::RawFact,
        effects: vec![EffectScope::MemoryWrite, EffectScope::ProjectCycle],
        projection_hints: vec![ProjectionKind::Timeline, ProjectionKind::ProjectCycle],
        payload: json!({
            "conversation_id": conversation_id,
            "turn_id": turn_id,
            "agent_id": agent_id,
            "path_prefix": path_prefix,
            "captured_memory_ids": captured_memory_ids,
            "message_count": message_count,
        }),
        provenance: json!({
            "source": "capture_session",
            "note": "raw capture marker; no pattern counter update implied",
        }),
        created_at: now_rfc3339(),
    };

    match write_event(server, target, &event) {
        Ok(()) => json!({"status": "saved", "event_id": event.id, "event_type": event.event_type}),
        Err(error) => json!({"status": "failed", "error": error, "event_type": event.event_type}),
    }
}

fn infer_saved_memory_projection_hints(entry: &MemoryEntry) -> Vec<ProjectionKind> {
    let path = entry.path.trim().to_ascii_lowercase();
    if path.starts_with("/user/patterns/bonding") {
        return vec![ProjectionKind::Bonding];
    }
    if path.starts_with("/user/patterns") {
        return vec![ProjectionKind::Pattern];
    }
    if path.starts_with("/user/affect") {
        return vec![ProjectionKind::Affect];
    }
    if path.starts_with("/lorebook") {
        return vec![ProjectionKind::WorldBook];
    }
    if path.starts_with("/timeline") {
        return vec![ProjectionKind::Timeline];
    }
    if path.starts_with("/outcomes") {
        return vec![ProjectionKind::Outcome];
    }
    if path.starts_with("/project-cycle") {
        return vec![ProjectionKind::ProjectCycle];
    }
    if path.starts_with("/domain-profile") {
        return vec![ProjectionKind::DomainProfile];
    }
    if path.starts_with("/evidence-gates") {
        return vec![ProjectionKind::EvidenceGate];
    }

    match entry.category.trim().to_ascii_lowercase().as_str() {
        "preference" => vec![ProjectionKind::Pattern],
        "decision" => vec![ProjectionKind::Outcome],
        "experience" => vec![ProjectionKind::Timeline],
        "entity" => vec![ProjectionKind::DomainProfile],
        _ => Vec::new(),
    }
}

pub(crate) fn emit_memory_saved_event(
    server: &MemoryServer,
    entry: &MemoryEntry,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Value {
    let target = if let Some(project) = named_project
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        ContinuityEventTarget::new(DbScope::Project, Some(project.to_string()), None)
    } else {
        ContinuityEventTarget::new(target_db, None, None)
    };
    let projections = infer_saved_memory_projection_hints(entry);
    let event = TachiEventRecord {
        id: stable_event_payload_id(&[
            "memory.saved",
            entry.id.as_str(),
            entry.path.as_str(),
            entry.timestamp.as_str(),
        ]),
        source_repo: "tachi".to_string(),
        adapter: "save_memory".to_string(),
        project: target.project_label(named_project),
        domain: entry
            .domain
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| entry.category.clone()),
        session_id: entry.id.clone(),
        actor: "tachi_memory".to_string(),
        event_type: "memory.saved".to_string(),
        authority: AuthorityLevel::RawFact,
        effects: vec![EffectScope::MemoryWrite],
        projection_hints: projections,
        payload: json!({
            "memory_id": entry.id,
            "path": entry.path,
            "summary": entry.summary,
            "text": entry.text,
            "category": entry.category,
            "topic": entry.topic,
            "keywords": entry.keywords,
            "entities": entry.entities,
            "scope": entry.scope,
            "tier": entry.tier,
            "metadata": entry.metadata,
        }),
        provenance: json!({
            "source": "save_memory",
            "note": "raw memory save marker; projection hints are path/category-derived and candidate hits are not implied",
        }),
        created_at: now_rfc3339(),
    };

    match write_event(server, &target, &event) {
        Ok(()) => json!({
            "status": "saved",
            "event_id": event.id,
            "event_type": event.event_type,
            "projection_hints": event.projection_hints.iter().map(|projection| projection.as_str()).collect::<Vec<_>>(),
        }),
        Err(error) => json!({"status": "failed", "error": error, "event_type": event.event_type}),
    }
}

fn pattern_projection_from_str(value: Option<&str>) -> ProjectionKind {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("bonding") => ProjectionKind::Bonding,
        _ => ProjectionKind::Pattern,
    }
}

fn feedback_event_type(projection: ProjectionKind, outcome: &str) -> String {
    let prefix = match projection {
        ProjectionKind::Bonding => "bonding",
        _ => "pattern",
    };
    format!("{prefix}.{outcome}")
}

fn pattern_row_projection_key(row: &Value) -> Option<&str> {
    row.get("metadata")
        .and_then(|metadata| metadata.get("projection_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn pattern_row_projection(row: &Value) -> ProjectionKind {
    pattern_projection_from_str(
        row.get("metadata")
            .and_then(|metadata| metadata.get("projection_kind"))
            .and_then(Value::as_str),
    )
}

fn pattern_row_id(row: &Value) -> Option<&str> {
    row.get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn emit_pattern_feedback_event(
    server: &MemoryServer,
    project: Option<&str>,
    pattern_id: &str,
    projection_key: &str,
    projection: ProjectionKind,
    outcome: &str,
    query: Option<&str>,
    note: Option<&str>,
    source: Option<&str>,
    metadata: Option<Value>,
) -> Result<Value, String> {
    let outcome = outcome.trim().to_ascii_lowercase();
    if !matches!(outcome.as_str(), "seen" | "hit" | "miss" | "stale") {
        return Err(format!(
            "invalid pattern feedback outcome '{outcome}'; expected seen, hit, miss, or stale"
        ));
    }
    let projection_key = projection_key.trim();
    if projection_key.is_empty() {
        return Err("projection_key is required for pattern feedback".to_string());
    }

    let target = ContinuityEventTarget::from_default_write(server, project);
    let projection_name = projection.as_str().to_string();
    let event_type = feedback_event_type(projection, &outcome);
    let event = TachiEventRecord {
        id: format!("event-{}", uuid::Uuid::new_v4()),
        source_repo: "tachi".to_string(),
        adapter: source
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("pattern_feedback")
            .to_string(),
        project: target.project_label(project),
        domain: "pattern_memory".to_string(),
        session_id: query.unwrap_or_default().to_string(),
        actor: "tachi_memory".to_string(),
        event_type: event_type.clone(),
        authority: if outcome == "seen" {
            AuthorityLevel::CollectOnly
        } else {
            AuthorityLevel::ReviewSignalOnly
        },
        effects: if outcome == "seen" {
            vec![EffectScope::Recall]
        } else {
            vec![EffectScope::Recall, EffectScope::Scoring]
        },
        projection_hints: vec![projection],
        payload: json!({
            "projection_key": projection_key,
            "pattern_id": pattern_id,
            "outcome": outcome,
            "query": query.unwrap_or_default(),
            "note": note.unwrap_or_default(),
            "metadata": metadata.unwrap_or_else(|| json!({})),
        }),
        provenance: json!({
            "source": "pattern_feedback",
            "note": "pattern feedback updates continuity projection counters via the event ledger",
        }),
        created_at: now_rfc3339(),
    };

    write_event(server, &target, &event)?;
    let projection_report =
        super::projection::project_auto_continuity_events_for_target(server, target, 50)?;
    Ok(json!({
        "status": "saved",
        "event_id": event.id,
        "event_type": event_type,
        "pattern_id": pattern_id,
        "projection_key": projection_key,
        "projection": projection_name,
        "outcome": outcome,
        "projection_report": projection_report,
    }))
}

pub(crate) fn emit_pattern_seen_events(
    server: &MemoryServer,
    project: Option<&str>,
    query: Option<&str>,
    rows: &[Value],
) -> Value {
    let mut seen_keys = HashSet::new();
    let mut saved = Vec::new();
    let mut errors = Vec::new();
    for row in rows {
        let Some(projection_key) = pattern_row_projection_key(row) else {
            continue;
        };
        if !seen_keys.insert(projection_key.to_string()) {
            continue;
        }
        let pattern_id = pattern_row_id(row).unwrap_or(projection_key);
        let projection = pattern_row_projection(row);
        match emit_pattern_feedback_event(
            server,
            project,
            pattern_id,
            projection_key,
            projection,
            "seen",
            query,
            None,
            Some("tachi_search"),
            None,
        ) {
            Ok(value) => saved.push(value),
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

pub(crate) struct WikiSavedEventInput<'a> {
    pub project: Option<&'a str>,
    pub wiki_id: &'a str,
    pub path: &'a str,
    pub title: &'a str,
    pub topic: &'a str,
    pub summary: &'a str,
    pub text: &'a str,
    pub domain: Option<&'a str>,
    pub mode: &'a str,
    pub references: &'a [String],
    pub pattern_refs: &'a [Value],
}

pub(crate) fn emit_wiki_saved_event(
    server: &MemoryServer,
    input: WikiSavedEventInput<'_>,
) -> Value {
    let target = ContinuityEventTarget::from_default_write(server, input.project);
    let event = TachiEventRecord {
        id: stable_event_payload_id(&[
            "wiki.saved",
            input.wiki_id,
            input.path,
            input.mode,
            &uuid::Uuid::new_v4().to_string(),
        ]),
        source_repo: "tachi".to_string(),
        adapter: "tachi_wiki_write".to_string(),
        project: target.project_label(input.project),
        domain: input.domain.unwrap_or("wiki").to_string(),
        session_id: input.wiki_id.to_string(),
        actor: "tachi_wiki".to_string(),
        event_type: "wiki.saved".to_string(),
        authority: AuthorityLevel::RawFact,
        effects: vec![
            EffectScope::MemoryWrite,
            EffectScope::Recall,
            EffectScope::Prompt,
        ],
        projection_hints: vec![ProjectionKind::Timeline, ProjectionKind::ProjectCycle],
        payload: json!({
            "wiki_id": input.wiki_id,
            "path": input.path,
            "title": input.title,
            "topic": input.topic,
            "summary": input.summary,
            "text": input.text,
            "mode": input.mode,
            "references": input.references,
            "pattern_refs": input.pattern_refs,
        }),
        provenance: json!({
            "source": "tachi_wiki_write",
            "note": "reviewed wiki write returned to the continuity ledger; projection is a read-model marker, not a replacement for the wiki row",
        }),
        created_at: now_rfc3339(),
    };

    match write_event(server, &target, &event) {
        Ok(()) => json!({
            "status": "saved",
            "event_id": event.id,
            "event_type": event.event_type,
            "projection_hints": event.projection_hints.iter().map(|projection| projection.as_str()).collect::<Vec<_>>(),
        }),
        Err(error) => json!({"status": "failed", "error": error, "event_type": event.event_type}),
    }
}

pub(crate) fn emit_task_completion_events(
    server: &MemoryServer,
    task_payload: Value,
    subagents: &[Value],
    project: Option<&str>,
) -> Value {
    let target = ContinuityEventTarget::from_default_write(server, project);
    let task_id = task_payload
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let agent = task_payload
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or("agent")
        .to_string();
    let task_event = TachiEventRecord {
        id: stable_event_payload_id(&[
            "task.outcome",
            task_id.as_str(),
            agent.as_str(),
            &uuid::Uuid::new_v4().to_string(),
        ]),
        source_repo: "tachi".to_string(),
        adapter: "tachi_complete".to_string(),
        project: target.project_label(project),
        domain: task_payload
            .get("task_type")
            .and_then(Value::as_str)
            .unwrap_or("task")
            .to_string(),
        session_id: task_id.to_string(),
        actor: agent.to_string(),
        event_type: "task.outcome".to_string(),
        authority: AuthorityLevel::ReviewSignalOnly,
        effects: vec![
            EffectScope::Scoring,
            EffectScope::Routing,
            EffectScope::ProjectCycle,
        ],
        projection_hints: vec![ProjectionKind::Outcome, ProjectionKind::ProjectCycle],
        payload: task_payload,
        provenance: json!({
            "source": "tachi_complete",
            "note": "task eval bridge from /eval memory into append-only event ledger",
        }),
        created_at: now_rfc3339(),
    };

    let mut saved = Vec::new();
    let mut errors = Vec::new();
    match write_event(server, &target, &task_event) {
        Ok(()) => saved.push(json!({"id": task_event.id, "event_type": task_event.event_type})),
        Err(error) => errors.push(json!({"event_type": task_event.event_type, "error": error})),
    }

    for (index, subagent) in subagents.iter().enumerate() {
        let role = subagent
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("subagent");
        let subagent_name = subagent
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("agent");
        let event = TachiEventRecord {
            id: stable_event_payload_id(&[
                "subagent.evaluated",
                task_id.as_str(),
                role,
                subagent_name,
                &index.to_string(),
                &uuid::Uuid::new_v4().to_string(),
            ]),
            source_repo: "tachi".to_string(),
            adapter: "tachi_complete".to_string(),
            project: target.project_label(project),
            domain: subagent
                .get("task_type")
                .and_then(Value::as_str)
                .unwrap_or("subagent")
                .to_string(),
            session_id: task_id.to_string(),
            actor: subagent_name.to_string(),
            event_type: "subagent.evaluated".to_string(),
            authority: AuthorityLevel::ReviewSignalOnly,
            effects: vec![EffectScope::Scoring, EffectScope::Routing],
            projection_hints: vec![ProjectionKind::DomainProfile, ProjectionKind::ProjectCycle],
            payload: json!({
                "task_id": task_id.clone(),
                "parent_agent": agent.clone(),
                "subagent": subagent,
            }),
            provenance: json!({
                "source": "tachi_complete.subagents",
                "note": "leader-compressed subagent eval; raw transcripts stay out of memory",
            }),
            created_at: now_rfc3339(),
        };
        match write_event(server, &target, &event) {
            Ok(()) => saved.push(json!({"id": event.id, "event_type": event.event_type})),
            Err(error) => errors.push(json!({"event_type": event.event_type, "error": error})),
        }
    }

    json!({
        "status": if errors.is_empty() { "saved" } else { "partial" },
        "saved_count": saved.len(),
        "events": saved,
        "errors": errors,
    })
}
