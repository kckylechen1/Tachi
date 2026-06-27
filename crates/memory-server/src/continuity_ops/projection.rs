use memory_core::{AuthorityLevel, MemoryEntry, ProjectionKind, TachiEventQuery, TachiEventRecord};
use serde_json::Map;
use serde_json::{json, Value};

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;

use super::storage::{get_projection_memory, read_events, upsert_projection_memory};
use super::{
    event_query_from_params, now_rfc3339, query_limit, target_from_event_params,
    ContinuityEventTarget,
};

pub(super) fn projection_kind_metadata(entry: &MemoryEntry) -> Option<&str> {
    entry
        .metadata
        .get("projection_kind")
        .and_then(Value::as_str)
}

pub(super) fn projection_filters(values: &[String]) -> Result<Vec<ProjectionKind>, String> {
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

pub(super) fn projected_path_prefix(projection: ProjectionKind) -> &'static str {
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
