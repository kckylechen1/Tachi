use chrono::{SecondsFormat, Utc};
use memory_core::{
    AuthorityLevel, EffectScope, MemoryEntry, OutcomeEvidenceBasis, ProjectionKind,
    SessionOutcomeKind, TachiEventQuery, TachiEventRecord,
};
use serde_json::Map;
use serde_json::{json, Value};

use crate::tool_params::Message;
use crate::tool_params::TachiEventParams;
use crate::{DbScope, MemoryServer};

mod emit;
mod parsing;
mod storage;

pub(crate) use self::emit::{
    emit_memory_saved_event, emit_session_captured_event, emit_task_completion_events,
    emit_wiki_saved_event, WikiSavedEventInput,
};
pub(crate) use self::parsing::{parse_continuity_candidate_batch, parse_continuity_outcome_label};
pub(crate) use self::storage::ContinuityEventTarget;
use self::storage::{
    continuity_metrics, get_projection_memory, list_projection_memories, read_events,
    upsert_projection_memory, write_event,
};

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn stable_event_payload_id(parts: &[&str]) -> String {
    let joined = parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("|");
    format!("event-{}", crate::utils::stable_hash(&joined))
}

fn projection_candidate_event_type(projection: ProjectionKind) -> &'static str {
    match projection {
        ProjectionKind::Pattern => "pattern.candidate",
        ProjectionKind::Timeline => "timeline.candidate",
        ProjectionKind::Outcome => "outcome.candidate",
        ProjectionKind::Affect => "affect.candidate",
        ProjectionKind::Bonding => "bonding.candidate",
        ProjectionKind::WorldBook => "world_book.candidate",
        ProjectionKind::ProjectCycle => "project_cycle.candidate",
        ProjectionKind::DomainProfile => "domain_profile.candidate",
        ProjectionKind::EvidenceGate => "evidence_gate.candidate",
    }
}

fn query_limit(limit: usize) -> usize {
    limit.clamp(1, 500)
}

fn trim_opt(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn target_from_event_params(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> ContinuityEventTarget {
    if let Some(project) = trim_opt(&params.project) {
        ContinuityEventTarget::new(DbScope::Project, Some(project), None)
    } else if server.has_project_db() {
        ContinuityEventTarget::new(DbScope::Project, None, None)
    } else {
        ContinuityEventTarget::new(DbScope::Global, None, None)
    }
}

fn projection_kind_metadata(entry: &MemoryEntry) -> Option<&str> {
    entry
        .metadata
        .get("projection_kind")
        .and_then(Value::as_str)
}

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

fn projection_filters(values: &[String]) -> Result<Vec<ProjectionKind>, String> {
    let mut filters = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let projection = ProjectionKind::from_str_opt(Some(trimmed))
            .ok_or_else(|| format!("invalid projection filter: {trimmed}"))?;
        if !filters.contains(&projection) {
            filters.push(projection);
        }
    }
    Ok(filters)
}

fn inferred_projection_from_event_type(event_type: &str) -> Option<ProjectionKind> {
    let normalized = event_type.trim().to_ascii_lowercase();
    if normalized.starts_with("pattern.") {
        Some(ProjectionKind::Pattern)
    } else if normalized.starts_with("timeline.")
        || normalized == "session.captured"
        || normalized == "wiki.saved"
    {
        Some(ProjectionKind::Timeline)
    } else if normalized.starts_with("bonding.") {
        Some(ProjectionKind::Bonding)
    } else if normalized.starts_with("affect.") || normalized.starts_with("emotion.") {
        Some(ProjectionKind::Affect)
    } else if normalized.starts_with("world_book.")
        || normalized.starts_with("worldbook.")
        || normalized.starts_with("lorebook.")
    {
        Some(ProjectionKind::WorldBook)
    } else if normalized.starts_with("project_cycle.") || normalized == "task.outcome" {
        Some(ProjectionKind::ProjectCycle)
    } else if normalized.starts_with("domain_profile.") || normalized == "subagent.evaluated" {
        Some(ProjectionKind::DomainProfile)
    } else if normalized.starts_with("outcome.") || normalized == "session.outcome" {
        Some(ProjectionKind::Outcome)
    } else if normalized.starts_with("evidence_gate.") {
        Some(ProjectionKind::EvidenceGate)
    } else {
        None
    }
}

fn event_projections(event: &TachiEventRecord) -> Vec<ProjectionKind> {
    let mut projections = event.projection_hints.clone();
    if projections.is_empty() {
        if let Some(inferred) = inferred_projection_from_event_type(&event.event_type) {
            projections.push(inferred);
        }
    }
    projections
}

fn projected_path_prefix(projection: ProjectionKind) -> &'static str {
    match projection {
        ProjectionKind::Pattern => "/user/patterns",
        ProjectionKind::Timeline => "/timeline",
        ProjectionKind::Outcome => "/outcomes",
        ProjectionKind::Affect => "/user/affect",
        ProjectionKind::Bonding => "/user/patterns/bonding",
        ProjectionKind::WorldBook => "/lorebook",
        ProjectionKind::ProjectCycle => "/project-cycle",
        ProjectionKind::DomainProfile => "/domain-profile",
        ProjectionKind::EvidenceGate => "/evidence-gates",
    }
}

fn projection_category(projection: ProjectionKind) -> &'static str {
    match projection {
        ProjectionKind::Pattern | ProjectionKind::Bonding | ProjectionKind::Affect => "preference",
        ProjectionKind::WorldBook | ProjectionKind::DomainProfile => "entity",
        ProjectionKind::Outcome | ProjectionKind::EvidenceGate => "decision",
        ProjectionKind::Timeline | ProjectionKind::ProjectCycle => "experience",
    }
}

fn projection_scope(projection: ProjectionKind) -> &'static str {
    match projection {
        ProjectionKind::Pattern | ProjectionKind::Bonding | ProjectionKind::Affect => "user",
        _ => "project",
    }
}

fn slug_segment(value: &str) -> String {
    let mut out = String::new();
    for ch in value.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if ch == '_' || ch == '-' {
            ch
        } else {
            '-'
        };
        if mapped == '-' && out.ends_with('-') {
            continue;
        }
        out.push(mapped);
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "general".to_string()
    } else {
        out
    }
}

fn nested_payload(event: &TachiEventRecord) -> &Value {
    event.payload.get("candidate").unwrap_or(&event.payload)
}

fn nested_metadata(value: &Value) -> Value {
    value.get("metadata").cloned().unwrap_or_else(|| json!({}))
}

fn string_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn projection_key(event: &TachiEventRecord, projection: ProjectionKind) -> String {
    let payload = nested_payload(event);
    let metadata = nested_metadata(payload);
    let explicit = string_value(
        payload,
        &[
            "projection_key",
            "pattern_key",
            "bonding_key",
            "lorebook_key",
            "world_key",
            "emotion_key",
            "affect_key",
            "key",
            "name",
            "title",
            "summary",
        ],
    )
    .or_else(|| {
        string_value(
            &metadata,
            &[
                "projection_key",
                "pattern_key",
                "bonding_key",
                "lorebook_key",
                "world_key",
                "key",
                "name",
            ],
        )
    });
    explicit
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}:{}", projection.as_str(), event.id))
}

fn projection_memory_id(projection: ProjectionKind, key: &str) -> String {
    format!(
        "projection-{}-{}",
        projection.as_str(),
        crate::utils::stable_hash(key)
    )
}

fn projection_path(event: &TachiEventRecord, projection: ProjectionKind, key: &str) -> String {
    let domain = if event.domain.trim().is_empty() {
        event.project.as_str()
    } else {
        event.domain.as_str()
    };
    let domain = slug_segment(domain);
    let key_hash = crate::utils::stable_hash(key);
    format!(
        "{}/{}/{}",
        projected_path_prefix(projection),
        domain,
        &key_hash[..12]
    )
}

fn projected_summary(event: &TachiEventRecord, projection: ProjectionKind) -> String {
    let payload = nested_payload(event);
    string_value(payload, &["summary", "title", "state", "outcome", "label"])
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{} projection from {}",
                projection.as_str(),
                event.event_type
            )
        })
}

fn projected_text(event: &TachiEventRecord, projection: ProjectionKind) -> String {
    let payload = nested_payload(event);
    let summary = projected_summary(event, projection);
    string_value(
        payload,
        &[
            "text",
            "content",
            "body",
            "detail",
            "rationale",
            "reason",
            "snapshot_summary",
        ],
    )
    .map(str::to_string)
    .unwrap_or(summary)
}

fn projected_event_ids(metadata: &Value) -> Vec<String> {
    metadata
        .get("projected_event_ids")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn counter_i64(metadata: &Value, key: &str) -> i64 {
    metadata
        .get("counters")
        .and_then(|value| value.get(key))
        .and_then(Value::as_i64)
        .unwrap_or_default()
}

fn counter_delta(event_type: &str, projection: ProjectionKind) -> (i64, i64, i64) {
    let event_type = event_type.to_ascii_lowercase();
    if event_type.contains(".hit") || event_type.contains("callback_hit") {
        (1, 1, 0)
    } else if event_type.contains(".miss")
        || event_type.contains(".stale")
        || event_type.contains(".failed")
        || event_type.contains(".failure")
    {
        (1, 0, 1)
    } else if matches!(
        projection,
        ProjectionKind::Pattern
            | ProjectionKind::Bonding
            | ProjectionKind::WorldBook
            | ProjectionKind::Affect
    ) && (event_type.contains(".observed") || event_type.contains(".candidate"))
    {
        (1, 0, 0)
    } else {
        (0, 0, 0)
    }
}

fn projection_tier(
    existing: Option<&MemoryEntry>,
    projection: ProjectionKind,
    authority: AuthorityLevel,
    payload: &Value,
    seen: i64,
    hit: i64,
) -> String {
    if existing
        .map(|entry| entry.tier.as_str() == "pattern")
        .unwrap_or(false)
    {
        return "pattern".to_string();
    }
    let explicit_promote = payload
        .get("promote")
        .or_else(|| payload.get("manual_promotion"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if matches!(
        projection,
        ProjectionKind::Pattern | ProjectionKind::Bonding | ProjectionKind::WorldBook
    ) && (authority.is_decision_eligible() || explicit_promote)
    {
        "pattern".to_string()
    } else if matches!(
        projection,
        ProjectionKind::Pattern | ProjectionKind::Bonding
    ) && seen >= 3
        && hit > 0
    {
        "consolidated".to_string()
    } else {
        existing
            .map(|entry| entry.tier.clone())
            .unwrap_or_else(|| "raw".to_string())
    }
}

fn projection_importance(projection: ProjectionKind, authority: AuthorityLevel) -> f64 {
    let base = match projection {
        ProjectionKind::Pattern | ProjectionKind::Bonding | ProjectionKind::WorldBook => 0.72,
        ProjectionKind::Timeline | ProjectionKind::ProjectCycle => 0.62,
        ProjectionKind::Outcome | ProjectionKind::EvidenceGate | ProjectionKind::DomainProfile => {
            0.58
        }
        ProjectionKind::Affect => 0.5,
    };
    if authority.is_decision_eligible() {
        (base + 0.08_f64).min(0.9_f64)
    } else {
        base
    }
}

fn projection_retention(projection: ProjectionKind, authority: AuthorityLevel) -> Option<String> {
    if matches!(
        projection,
        ProjectionKind::Pattern | ProjectionKind::Bonding | ProjectionKind::WorldBook
    ) && authority.is_decision_eligible()
    {
        Some("permanent".to_string())
    } else {
        Some("durable".to_string())
    }
}

fn projection_metadata(
    existing: Option<&MemoryEntry>,
    event: &TachiEventRecord,
    projection: ProjectionKind,
    key: &str,
) -> (Value, bool, i64, i64, i64) {
    let payload = nested_payload(event);
    let mut metadata = existing
        .and_then(|entry| entry.metadata.as_object().cloned())
        .unwrap_or_default();
    let mut ids = existing
        .map(|entry| projected_event_ids(&entry.metadata))
        .unwrap_or_default();
    let already_projected = ids.iter().any(|id| id == &event.id);
    if !already_projected {
        ids.push(event.id.clone());
    }

    let (seen_delta, hit_delta, miss_delta) = if already_projected {
        (0, 0, 0)
    } else {
        counter_delta(&event.event_type, projection)
    };
    let seen = counter_i64(&Value::Object(metadata.clone()), "seen") + seen_delta;
    let hit = counter_i64(&Value::Object(metadata.clone()), "hit") + hit_delta;
    let miss = counter_i64(&Value::Object(metadata.clone()), "miss") + miss_delta;
    let confidence = if seen > 0 {
        Some(hit as f64 / seen as f64)
    } else {
        payload
            .get("confidence")
            .or_else(|| payload.get("score"))
            .and_then(Value::as_f64)
            .map(|value| value.clamp(0.0, 1.0))
    };

    metadata.insert("projection_kind".to_string(), json!(projection.as_str()));
    metadata.insert("projection_key".to_string(), json!(key));
    metadata.insert("source_event_id".to_string(), json!(event.id));
    metadata.insert("source_event_type".to_string(), json!(event.event_type));
    metadata.insert("source_repo".to_string(), json!(event.source_repo));
    metadata.insert("adapter".to_string(), json!(event.adapter));
    metadata.insert("authority".to_string(), json!(event.authority.as_str()));
    metadata.insert(
        "effects".to_string(),
        json!(event
            .effects
            .iter()
            .map(|effect| effect.as_str())
            .collect::<Vec<_>>()),
    );
    metadata.insert("projected_event_ids".to_string(), json!(ids));
    metadata.insert(
        "counters".to_string(),
        json!({
            "seen": seen,
            "hit": hit,
            "miss": miss,
            "confidence": confidence,
            "last_seen": event.created_at,
        }),
    );
    if projection == ProjectionKind::Affect {
        metadata.insert(
            "guardrails".to_string(),
            json!({
                "authority": "tone_and_reminder_only",
                "live_effect": "tone_only",
                "score_effect": "none",
                "iron_effect": "none",
                "execution_effect": "none",
                "portfolio_effect": "none",
            }),
        );
    }
    if projection == ProjectionKind::WorldBook {
        let mut lorebook = Map::new();
        for key in [
            "keys",
            "secondary_keys",
            "position",
            "priority",
            "token_budget",
            "constant",
            "selective",
            "recursive",
            "enabled",
        ] {
            if let Some(value) = payload.get(key) {
                lorebook.insert(key.to_string(), value.clone());
            }
        }
        if !lorebook.is_empty() {
            metadata.insert("lorebook".to_string(), Value::Object(lorebook));
        }
    }

    (Value::Object(metadata), already_projected, seen, hit, miss)
}

fn projection_keywords(
    event: &TachiEventRecord,
    projection: ProjectionKind,
    key: &str,
) -> Vec<String> {
    let mut keywords = vec![
        "continuity".to_string(),
        projection.as_str().to_string(),
        event.event_type.clone(),
    ];
    if !event.domain.trim().is_empty() {
        keywords.push(event.domain.clone());
    }
    if !key.trim().is_empty() {
        keywords.push(slug_segment(key));
    }
    keywords.sort();
    keywords.dedup();
    keywords
}

fn build_projection_entry(
    existing: Option<MemoryEntry>,
    event: &TachiEventRecord,
    projection: ProjectionKind,
) -> (MemoryEntry, bool) {
    let key = projection_key(event, projection);
    let id = projection_memory_id(projection, &key);
    let path = projection_path(event, projection, &key);
    let payload = nested_payload(event);
    let existing_ref = existing.as_ref();
    let (metadata, already_projected, seen, hit, _miss) =
        projection_metadata(existing_ref, event, projection, &key);
    let tier = projection_tier(
        existing_ref,
        projection,
        event.authority,
        payload,
        seen,
        hit,
    );
    let text = projected_text(event, projection);
    let summary = projected_summary(event, projection);
    let timestamp = if event.created_at.trim().is_empty() {
        now_rfc3339()
    } else {
        event.created_at.clone()
    };
    let entities = [
        event.actor.as_str(),
        event.project.as_str(),
        event.source_repo.as_str(),
    ]
    .into_iter()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_string)
    .collect::<Vec<_>>();
    let mut entry = existing.unwrap_or_else(|| MemoryEntry {
        id: id.clone(),
        path: path.clone(),
        summary: summary.clone(),
        text: text.clone(),
        importance: projection_importance(projection, event.authority),
        timestamp: timestamp.clone(),
        valid_from: timestamp.clone(),
        valid_until: None,
        category: projection_category(projection).to_string(),
        topic: projection.as_str().to_string(),
        keywords: projection_keywords(event, projection, &key),
        persons: Vec::new(),
        entities: entities.clone(),
        location: String::new(),
        source: "external:tachi_event_projection".to_string(),
        scope: projection_scope(projection).to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        retention_policy: projection_retention(projection, event.authority),
        domain: (!event.domain.trim().is_empty()).then(|| event.domain.clone()),
        metadata: json!({}),
        recall_count: 0,
        query_diversity: 0,
        tier: tier.clone(),
    });
    entry.id = id;
    entry.path = path;
    entry.summary = summary;
    entry.text = text;
    entry.importance = projection_importance(projection, event.authority);
    entry.category = projection_category(projection).to_string();
    entry.topic = projection.as_str().to_string();
    entry.keywords = projection_keywords(event, projection, &key);
    entry.entities = entities;
    entry.source = "external:tachi_event_projection".to_string();
    entry.scope = projection_scope(projection).to_string();
    entry.metadata = metadata;
    entry.retention_policy = projection_retention(projection, event.authority);
    entry.domain = (!event.domain.trim().is_empty()).then(|| event.domain.clone());
    entry.tier = tier;
    (entry, already_projected)
}

fn event_query_from_params(params: &TachiEventParams) -> TachiEventQuery {
    TachiEventQuery {
        project: trim_opt(&params.project),
        domain: trim_opt(&params.domain),
        event_type: trim_opt(&params.event_type),
        session_id: trim_opt(&params.session_id),
        source_repo: trim_opt(&params.source_repo),
        adapter: trim_opt(&params.adapter),
        limit: query_limit(params.limit),
    }
}

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

fn payload_string<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn review_target(review: &TachiEventRecord) -> Option<&str> {
    payload_string(
        &review.payload,
        &["target_event_id", "event_id", "label_event_id"],
    )
}

fn gold_outcome(review: &TachiEventRecord) -> SessionOutcomeKind {
    SessionOutcomeKind::from_str_opt(payload_string(
        &review.payload,
        &["gold_outcome", "outcome", "expected_outcome", "label"],
    ))
}

fn gold_basis(review: &TachiEventRecord) -> OutcomeEvidenceBasis {
    OutcomeEvidenceBasis::from_str_opt(payload_string(
        &review.payload,
        &[
            "gold_evidence_basis",
            "evidence_basis",
            "expected_evidence_basis",
            "basis",
        ],
    ))
}

pub(crate) fn evaluate_outcome_labels(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> Result<Value, String> {
    let target = target_from_event_params(server, params);
    let limit = query_limit(params.limit);
    let mut outcome_query = event_query_from_params(params);
    outcome_query.event_type = Some("session.outcome".to_string());
    outcome_query.limit = limit;
    let outcomes = read_events(server, &target, &outcome_query)?;

    let mut review_query = event_query_from_params(params);
    review_query.event_type = Some("session.outcome.review".to_string());
    review_query.limit = limit;
    let reviews = read_events(server, &target, &review_query)?;

    let mut reviewed = 0usize;
    let mut outcome_matches = 0usize;
    let mut basis_matches = 0usize;
    let mut full_matches = 0usize;
    let mut missing_targets = 0usize;
    let mut rows = Vec::new();

    for review in reviews {
        let target_event_id = review_target(&review).map(str::to_string);
        let matched = target_event_id
            .as_deref()
            .and_then(|id| outcomes.iter().find(|event| event.id == id))
            .or_else(|| {
                outcomes.iter().find(|event| {
                    !review.session_id.is_empty() && event.session_id == review.session_id
                })
            });

        let Some(label_event) = matched else {
            missing_targets += 1;
            rows.push(json!({
                "review_event_id": review.id,
                "target_event_id": target_event_id,
                "session_id": review.session_id,
                "status": "missing_target",
            }));
            continue;
        };

        reviewed += 1;
        let expected_outcome = gold_outcome(&review);
        let expected_basis = gold_basis(&review);
        let actual_outcome = SessionOutcomeKind::from_str_opt(payload_string(
            &label_event.payload,
            &["outcome", "outcome_label", "label"],
        ));
        let actual_basis = OutcomeEvidenceBasis::from_str_opt(payload_string(
            &label_event.payload,
            &[
                "evidence_basis",
                "basis",
                "label_basis",
                "adversarial_basis",
            ],
        ));
        let outcome_match =
            expected_outcome != SessionOutcomeKind::Unknown && expected_outcome == actual_outcome;
        let basis_match =
            expected_basis != OutcomeEvidenceBasis::Unverified && expected_basis == actual_basis;
        if outcome_match {
            outcome_matches += 1;
        }
        if basis_match {
            basis_matches += 1;
        }
        if outcome_match && basis_match {
            full_matches += 1;
        }
        rows.push(json!({
            "review_event_id": review.id,
            "label_event_id": label_event.id,
            "session_id": label_event.session_id,
            "expected": {
                "outcome": expected_outcome.as_str(),
                "evidence_basis": expected_basis.as_str(),
            },
            "actual": {
                "outcome": actual_outcome.as_str(),
                "evidence_basis": actual_basis.as_str(),
            },
            "outcome_match": outcome_match,
            "basis_match": basis_match,
        }));
    }

    let ratio = |count: usize| {
        if reviewed > 0 {
            Some(count as f64 / reviewed as f64)
        } else {
            None
        }
    };

    Ok(json!({
        "status": "completed",
        "reviewed": reviewed,
        "missing_targets": missing_targets,
        "outcome_accuracy": ratio(outcome_matches),
        "evidence_basis_accuracy": ratio(basis_matches),
        "full_match_rate": ratio(full_matches),
        "rows": rows,
        "note": "Read-only label-quality harness. Reviews are session.outcome.review events with target_event_id or matching session_id; no labels or counters are mutated.",
    }))
}

fn continuity_pipeline_enabled() -> bool {
    std::env::var("TACHI_CONTINUITY_PIPELINE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn maybe_spawn_session_continuity_pipeline(
    server: &MemoryServer,
    target: ContinuityEventTarget,
    conversation_id: String,
    turn_id: String,
    agent_id: String,
    project: Option<String>,
    messages: Vec<Message>,
) -> Value {
    if !continuity_pipeline_enabled() {
        return json!({
            "status": "disabled",
            "reason": "set TACHI_CONTINUITY_PIPELINE=1 to run distill/reasoning continuity labelers",
        });
    }

    let server_clone = server.clone();
    let event_count_hint = messages.len();
    tokio::spawn(async move {
        let payload = json!({
            "conversation_id": conversation_id,
            "turn_id": turn_id,
            "agent_id": agent_id,
            "messages": messages,
        });
        let request = match serde_json::to_string_pretty(&payload) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!("[continuity] serialize session payload failed: {error}");
                return;
            }
        };

        match server_clone
            .llm
            .call_distill_llm(
                crate::prompts::CONTINUITY_CANDIDATE_PROMPT,
                &request,
                None,
                0.2,
                1800,
            )
            .await
        {
            Ok(raw) => match parse_continuity_candidate_batch(&raw) {
                Ok(batch) => {
                    for candidate in batch.candidates {
                        let event_type = candidate
                            .event_type
                            .clone()
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or_else(|| {
                                projection_candidate_event_type(candidate.projection).to_string()
                            });
                        let event = TachiEventRecord {
                            id: stable_event_payload_id(&[
                                event_type.as_str(),
                                payload["conversation_id"].as_str().unwrap_or_default(),
                                candidate.summary.as_str(),
                                &uuid::Uuid::new_v4().to_string(),
                            ]),
                            source_repo: "tachi".to_string(),
                            adapter: "continuity_distill".to_string(),
                            project: target.project_label(project.as_deref()),
                            domain: "session".to_string(),
                            session_id: payload["conversation_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            actor: payload["agent_id"].as_str().unwrap_or_default().to_string(),
                            event_type,
                            authority: AuthorityLevel::CollectOnly,
                            effects: vec![EffectScope::None],
                            projection_hints: vec![candidate.projection],
                            payload: json!({
                                "candidate": candidate,
                                "auto_applied": false,
                            }),
                            provenance: json!({
                                "source": "continuity_distill",
                                "lane": "distill",
                            }),
                            created_at: now_rfc3339(),
                        };
                        if let Err(error) = write_event(&server_clone, &target, &event) {
                            tracing::warn!("[continuity] candidate event write failed: {error}");
                        }
                    }
                }
                Err(error) => tracing::warn!("[continuity] candidate parse failed: {error}"),
            },
            Err(error) => tracing::warn!("[continuity] candidate distill failed: {error}"),
        }

        match server_clone
            .llm
            .call_reasoning_llm(
                crate::prompts::SESSION_OUTCOME_LABEL_PROMPT,
                &request,
                None,
                0.0,
                900,
            )
            .await
        {
            Ok(raw) => match parse_continuity_outcome_label(&raw) {
                Ok(label) => {
                    let event = TachiEventRecord {
                        id: stable_event_payload_id(&[
                            "session.outcome",
                            payload["conversation_id"].as_str().unwrap_or_default(),
                            payload["turn_id"].as_str().unwrap_or_default(),
                            &uuid::Uuid::new_v4().to_string(),
                        ]),
                        source_repo: "tachi".to_string(),
                        adapter: "continuity_labeler".to_string(),
                        project: target.project_label(project.as_deref()),
                        domain: "session".to_string(),
                        session_id: payload["conversation_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        actor: payload["agent_id"].as_str().unwrap_or_default().to_string(),
                        event_type: "session.outcome".to_string(),
                        authority: AuthorityLevel::ReviewSignalOnly,
                        effects: vec![EffectScope::Scoring],
                        projection_hints: vec![
                            ProjectionKind::Outcome,
                            ProjectionKind::EvidenceGate,
                        ],
                        payload: json!({
                            "outcome": label.outcome.as_str(),
                            "evidence_basis": label.evidence_basis.as_str(),
                            "confidence": label.confidence,
                            "rationale": label.rationale,
                            "evidence_refs": label.evidence_refs,
                            "claims": label.claims,
                            "open_questions": label.open_questions,
                        }),
                        provenance: json!({
                            "source": "continuity_labeler",
                            "lane": "reasoning",
                            "note": "read-only signal; projectors must calibrate before automatic counter updates",
                        }),
                        created_at: now_rfc3339(),
                    };
                    if let Err(error) = write_event(&server_clone, &target, &event) {
                        tracing::warn!("[continuity] outcome event write failed: {error}");
                    }
                }
                Err(error) => tracing::warn!("[continuity] outcome parse failed: {error}"),
            },
            Err(error) => tracing::warn!("[continuity] outcome labeler failed: {error}"),
        }
    });

    json!({
        "status": "enqueued",
        "messages": event_count_hint,
        "lanes": ["distill", "reasoning"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_continuity_candidates_with_projection_aliases() {
        let raw = r#"{
          "candidates": [
            {
              "projection": "timeline",
              "summary": "User reframed scope",
              "text": "The session moved from ROI judgment to code mapping.",
              "confidence": 0.74,
              "evidence_refs": ["message:4"]
            },
            {
              "kind": "worldbook",
              "summary": "Tachi substrate",
              "metadata": {"domain": "agent_os"}
            }
          ],
          "open_threads": ["wire projectors"]
        }"#;

        let parsed = parse_continuity_candidate_batch(raw).expect("parse candidates");
        assert_eq!(parsed.candidates.len(), 2);
        assert_eq!(parsed.candidates[0].projection, ProjectionKind::Timeline);
        assert_eq!(parsed.candidates[1].projection, ProjectionKind::WorldBook);
        assert_eq!(parsed.open_threads, vec!["wire projectors"]);
    }

    #[test]
    fn parses_outcome_label_into_typed_axes() {
        let raw = r#"{
          "outcome": "partial_reframe",
          "evidence_basis": "interlocutor_argument",
          "confidence": 0.61,
          "rationale": "Both sides changed scope.",
          "claims": ["enum too coarse"],
          "open_questions": ["external label source"]
        }"#;

        let parsed = parse_continuity_outcome_label(raw).expect("parse outcome");
        assert_eq!(parsed.outcome, SessionOutcomeKind::PartialReframe);
        assert_eq!(
            parsed.evidence_basis,
            OutcomeEvidenceBasis::InterlocutorArgument
        );
        assert_eq!(parsed.claims, vec!["enum too coarse"]);
    }

    #[test]
    fn emits_session_captured_event_to_target_store() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("memory.db");
        let server = MemoryServer::new(db, None).expect("test server");
        let target = ContinuityEventTarget::new(DbScope::Global, None, None);
        let status = emit_session_captured_event(
            &server,
            &target,
            "conversation-1",
            "turn-1",
            "codex",
            "/agents/codex",
            &["memory-1".to_string()],
            3,
            Some("sigil"),
        );
        assert_eq!(status["status"], json!("saved"));

        let events = server
            .with_global_store_read(|store| {
                store
                    .list_tachi_events(&memory_core::TachiEventQuery {
                        event_type: Some("session.captured".to_string()),
                        session_id: Some("conversation-1".to_string()),
                        limit: 5,
                        ..memory_core::TachiEventQuery::default()
                    })
                    .map_err(|e| e.to_string())
            })
            .expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].project, "sigil");
        assert_eq!(
            events[0].projection_hints,
            vec![ProjectionKind::Timeline, ProjectionKind::ProjectCycle]
        );
        assert_eq!(
            events[0].payload["captured_memory_ids"][0],
            json!("memory-1")
        );
    }

    #[test]
    fn auto_projection_skips_execution_gate_events() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("memory.db");
        let server = MemoryServer::new(db, None).expect("test server");
        server
            .with_global_store(|store| {
                let allowed = TachiEventRecord {
                    id: "pattern-candidate-1".to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "sigil".to_string(),
                    domain: "agent_os".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "pattern.candidate".to_string(),
                    authority: AuthorityLevel::CollectOnly,
                    effects: vec![EffectScope::None],
                    projection_hints: vec![ProjectionKind::Pattern],
                    payload: json!({
                        "summary": "Continuity-first planning",
                        "text": "Use continuity evidence before picking the next project-management action.",
                        "projection_key": "continuity-first"
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                let blocked = TachiEventRecord {
                    id: "execution-gate-1".to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "sigil".to_string(),
                    domain: "agent_os".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "evidence_gate.required".to_string(),
                    authority: AuthorityLevel::ExecutionGate,
                    effects: vec![EffectScope::Execution],
                    projection_hints: vec![ProjectionKind::EvidenceGate],
                    payload: json!({
                        "summary": "Do not ship without tests",
                        "projection_key": "must-test"
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                store.insert_tachi_event(&allowed).map_err(|e| e.to_string())?;
                store.insert_tachi_event(&blocked).map_err(|e| e.to_string())
            })
            .expect("seed events");

        let report = project_auto_continuity_events_for_target(
            &server,
            ContinuityEventTarget::new(DbScope::Global, None, None),
            20,
        )
        .expect("project events");
        assert_eq!(report["projected_count"], json!(1));
        assert_eq!(report["skipped_count"], json!(1));

        let patterns = list_projection_memories(
            &server,
            &ContinuityEventTarget::new(DbScope::Global, None, None),
            "/user/patterns",
            10,
        )
        .expect("list patterns");
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].summary, "Continuity-first planning");
        let gates = list_projection_memories(
            &server,
            &ContinuityEventTarget::new(DbScope::Global, None, None),
            "/evidence-gates",
            10,
        )
        .expect("list gates");
        assert!(gates.is_empty());
    }
}
