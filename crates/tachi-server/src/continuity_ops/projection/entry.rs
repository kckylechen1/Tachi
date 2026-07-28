use memcore::{AuthorityLevel, MemoryEntry, ProjectionKind, TachiEventRecord};
use serde_json::Map;
use serde_json::{json, Value};

use super::super::now_rfc3339;
use super::super::read_models::{
    affect_projection_metadata, bonding_projection_metadata, timeline_projection_metadata,
};

pub(in crate::continuity_ops) fn projection_kind_metadata(entry: &MemoryEntry) -> Option<&str> {
    entry
        .metadata
        .get("projection_kind")
        .and_then(Value::as_str)
}

pub(in crate::continuity_ops) fn projection_filters(
    values: &[String],
) -> Result<Vec<ProjectionKind>, String> {
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

pub(super) fn event_projections(event: &TachiEventRecord) -> Vec<ProjectionKind> {
    let mut projections = event.projection_hints.clone();
    if projections.is_empty() {
        if let Some(inferred) = inferred_projection_from_event_type(&event.event_type) {
            projections.push(inferred);
        }
    }
    projections
}

pub(in crate::continuity_ops) fn projected_path_prefix(projection: ProjectionKind) -> &'static str {
    projection.path_prefix()
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

pub(super) fn projection_key(event: &TachiEventRecord, projection: ProjectionKind) -> String {
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

pub(super) fn projection_memory_id(projection: ProjectionKind, key: &str) -> String {
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

fn projection_domain_label(event: &TachiEventRecord, path: &str, category: &str) -> Option<String> {
    let raw = event.domain.trim();
    if raw.is_empty() {
        return None;
    }
    crate::repair::domain::repair_target(
        Some(raw),
        path,
        category,
        "external:tachi_event_projection",
    )
    .or_else(|| Some(raw.to_string()))
}

/// #1114 (codex round-1 B3 fix): the domain a projection WOULD carry,
/// computed the exact same way `build_projection_entry` computes
/// `entry.domain` (same `path`/`category` inputs), but WITHOUT building the
/// rest of the entry (keywords/entities/metadata JSON) — so the write
/// -affinity gate can be evaluated, and its routed destination resolved,
/// BEFORE the existing-row lookup that `build_projection_entry`'s merge
/// logic depends on. See `projection.rs`'s `resolve_projection_write_target`
/// for why the ordering matters (a stale pre-gate lookup silently resets a
/// rerouted projection's aggregation state on every subsequent run).
pub(in crate::continuity_ops) fn projection_event_domain(
    event: &TachiEventRecord,
    projection: ProjectionKind,
    key: &str,
) -> Option<String> {
    let path = projection_path(event, projection, key);
    let category = projection_category(projection);
    projection_domain_label(event, &path, category)
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

pub(super) fn counter_i64(metadata: &Value, key: &str) -> i64 {
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
    } else if (matches!(
        projection,
        ProjectionKind::Pattern
            | ProjectionKind::Bonding
            | ProjectionKind::WorldBook
            | ProjectionKind::Affect
    ) && (event_type.contains(".observed") || event_type.contains(".candidate")))
        || event_type.contains(".seen")
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
    miss: i64,
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
        && hit > miss
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
    let feedback = hit + miss;
    let confidence = if feedback > 0 {
        Some(hit as f64 / feedback as f64)
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
    if projection == ProjectionKind::Timeline {
        metadata.insert(
            "timeline".to_string(),
            timeline_projection_metadata(existing, event, payload),
        );
    }
    if projection == ProjectionKind::Bonding {
        metadata.insert(
            "lexicon".to_string(),
            bonding_projection_metadata(existing, event, payload, hit, hit_delta),
        );
    }
    if projection == ProjectionKind::Affect {
        let affect = affect_projection_metadata(event, payload);
        metadata.insert(
            "guardrails".to_string(),
            affect
                .get("guardrails")
                .cloned()
                .unwrap_or_else(|| json!({})),
        );
        metadata.insert("affect".to_string(), affect);
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

pub(super) fn build_projection_entry(
    existing: Option<MemoryEntry>,
    event: &TachiEventRecord,
    projection: ProjectionKind,
) -> (MemoryEntry, bool) {
    let key = projection_key(event, projection);
    let id = projection_memory_id(projection, &key);
    let path = projection_path(event, projection, &key);
    let payload = nested_payload(event);
    let existing_ref = existing.as_ref();
    let (metadata, already_projected, seen, hit, miss) =
        projection_metadata(existing_ref, event, projection, &key);
    let tier = projection_tier(
        existing_ref,
        projection,
        event.authority,
        payload,
        seen,
        hit,
        miss,
    );
    let text = projected_text(event, projection);
    let summary = projected_summary(event, projection);
    let category = projection_category(projection).to_string();
    let domain = projection_domain_label(event, &path, &category);
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
        category: category.clone(),
        topic: projection.as_str().to_string(),
        keywords: projection_keywords(event, projection, &key),
        persons: Vec::new(),
        entities: entities.clone(),
        location: String::new(),
        source: "external:tachi_event_projection".to_string(),
        scope: projection_scope(projection).to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: projection_retention(projection, event.authority),
        domain: domain.clone(),
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
    entry.category = category;
    entry.topic = projection.as_str().to_string();
    entry.keywords = projection_keywords(event, projection, &key);
    entry.entities = entities;
    entry.source = "external:tachi_event_projection".to_string();
    entry.scope = projection_scope(projection).to_string();
    entry.metadata = metadata;
    entry.retention_policy = projection_retention(projection, event.authority);
    entry.domain = domain;
    entry.tier = tier;
    (entry, already_projected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_with_domain(domain: &str) -> TachiEventRecord {
        TachiEventRecord {
            id: "event-1".to_string(),
            source_repo: "sigil".to_string(),
            adapter: "test".to_string(),
            project: "Sigil".to_string(),
            domain: domain.to_string(),
            session_id: "session-1".to_string(),
            actor: "codex".to_string(),
            event_type: "pattern.observed".to_string(),
            authority: AuthorityLevel::CollectOnly,
            effects: vec![memcore::EffectScope::None],
            projection_hints: vec![ProjectionKind::Pattern],
            payload: json!({"summary": "product pattern"}),
            provenance: json!({}),
            created_at: "2026-06-28T00:00:00Z".to_string(),
        }
    }

    fn bonding_event(id: &str, event_type: &str, created_at: &str) -> TachiEventRecord {
        TachiEventRecord {
            id: id.to_string(),
            source_repo: "sigil".to_string(),
            adapter: "test".to_string(),
            project: "Sigil".to_string(),
            domain: "agent_os".to_string(),
            session_id: "session-1".to_string(),
            actor: "codex".to_string(),
            event_type: event_type.to_string(),
            authority: AuthorityLevel::CollectOnly,
            effects: vec![memcore::EffectScope::None],
            projection_hints: vec![ProjectionKind::Bonding],
            payload: json!({
                "bonding_key": "shared-protocol",
                "summary": "Shared protocol",
                "meaning": "Shared shorthand and context",
            }),
            provenance: json!({}),
            created_at: created_at.to_string(),
        }
    }

    #[test]
    fn projection_domain_label_is_repair_clean() {
        let event = event_with_domain("product-test");
        let (entry, _) = build_projection_entry(None, &event, ProjectionKind::Pattern);
        assert_eq!(entry.domain.as_deref(), Some("product_test"));
        assert!(entry.path.contains("/product-test/"));
    }

    #[test]
    fn bonding_last_successful_use_only_updates_on_current_hit() {
        let observed = bonding_event(
            "bonding-observed-1",
            "bonding.observed",
            "2026-06-28T00:00:00Z",
        );
        let (entry, _) = build_projection_entry(None, &observed, ProjectionKind::Bonding);
        assert!(entry
            .metadata
            .get("lexicon")
            .and_then(|value| value.get("last_successful_use"))
            .is_none());

        let hit = bonding_event("bonding-hit-1", "bonding.hit", "2026-06-28T01:00:00Z");
        let (entry, _) = build_projection_entry(Some(entry), &hit, ProjectionKind::Bonding);
        assert_eq!(
            entry.metadata["lexicon"]["last_successful_use"],
            json!("2026-06-28T01:00:00Z")
        );

        let later_observed = bonding_event(
            "bonding-observed-2",
            "bonding.observed",
            "2026-06-28T02:00:00Z",
        );
        let (entry, _) =
            build_projection_entry(Some(entry), &later_observed, ProjectionKind::Bonding);
        assert_eq!(
            entry.metadata["lexicon"]["last_successful_use"],
            json!("2026-06-28T01:00:00Z")
        );
        assert_eq!(entry.metadata["lexicon"]["callback_hits"], json!(1));
    }
}
