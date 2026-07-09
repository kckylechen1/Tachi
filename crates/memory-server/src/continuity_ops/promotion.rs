use memory_core::{AuthorityLevel, EffectScope, MemoryEntry, ProjectionKind, TachiEventRecord};
use serde_json::{json, Value};

use crate::tool_params::{TachiEventParams, TachiSkillParams, WikiWriteParams};
use crate::MemoryServer;
use memory_server_runtime::trim_opt;

use super::context::list_active_patterns;
use super::feedback::pattern_ref_json;
use super::storage::write_event;
use super::{now_rfc3339, ContinuityEventTarget};

fn payload_bool(payload: Option<&Value>, key: &str) -> bool {
    payload
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn payload_string<'a>(payload: Option<&'a Value>, keys: &[&str]) -> Option<&'a str> {
    let payload = payload?;
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn pattern_ref_from_params(params: &TachiEventParams) -> Option<String> {
    trim_opt(&params.id).or_else(|| {
        payload_string(
            params.payload.as_ref(),
            &[
                "pattern_ref",
                "pattern_id",
                "projection_key",
                "path",
                "memory_id",
            ],
        )
        .map(str::to_string)
    })
}

fn ref_matches(entry: &MemoryEntry, pattern_ref: &str) -> bool {
    entry.id == pattern_ref
        || entry.path == pattern_ref
        || entry.metadata.get("projection_key").and_then(Value::as_str) == Some(pattern_ref)
}

fn entry_counter(metadata: &Value, key: &str) -> i64 {
    metadata
        .get("counters")
        .and_then(|counters| counters.get(key))
        .and_then(Value::as_i64)
        .or_else(|| metadata.get(key).and_then(Value::as_i64))
        .unwrap_or(0)
}

fn resolve_pattern(
    server: &MemoryServer,
    project: Option<&str>,
    pattern_ref: &str,
) -> Result<MemoryEntry, String> {
    let patterns = list_active_patterns(server, project, Some(pattern_ref), 100)?;
    patterns
        .into_iter()
        .find(|entry| ref_matches(entry, pattern_ref))
        .ok_or_else(|| format!("no active pattern matched '{pattern_ref}'"))
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

fn promotion_reason(entry: &MemoryEntry) -> Option<&'static str> {
    let projection = entry
        .metadata
        .get("projection_kind")
        .and_then(Value::as_str)?;
    if !matches!(projection, "pattern" | "bonding" | "world_book") {
        return None;
    }
    let seen = entry_counter(&entry.metadata, "seen");
    let hit = entry_counter(&entry.metadata, "hit");
    let miss = entry_counter(&entry.metadata, "miss");
    if seen >= 3 && hit > 0 && hit > miss {
        return Some("hit_threshold");
    }
    if entry.tier == "pattern" {
        return Some("reviewed_pattern_tier");
    }
    None
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
        "rule": "final promotion requires external validation and cold-seat review; review artifacts may still be created before final approval",
    })
}

fn review_markdown(entry: &MemoryEntry, reason: &str, gate: &Value) -> String {
    let pattern_ref = pattern_ref_json(entry);
    format!(
        "# Pattern Review: {}\n\n## Summary\n{}\n\n## Pattern\n{}\n\n## Counters\n```json\n{}\n```\n\n## Promotion Reason\n{}\n\n## Promotion Gate\n```json\n{}\n```\n\n## Pattern Ref\n```json\n{}\n```\n",
        entry.summary,
        entry.summary,
        entry.text,
        serde_json::to_string_pretty(
            entry
                .metadata
                .get("counters")
                .unwrap_or(&Value::Null)
        )
        .unwrap_or_else(|_| "{}".to_string()),
        reason,
        serde_json::to_string_pretty(gate).unwrap_or_else(|_| "{}".to_string()),
        serde_json::to_string_pretty(&pattern_ref).unwrap_or_else(|_| "{}".to_string()),
    )
}

async fn create_wiki_draft(
    server: &MemoryServer,
    params: &TachiEventParams,
    pattern: &MemoryEntry,
    reason: &str,
    gate: &Value,
    slug: &str,
) -> Result<Value, String> {
    let response = crate::copilot_ops::handle_tachi_wiki_write(
        server,
        WikiWriteParams {
            title: format!("Pattern Review: {}", pattern.summary),
            text: review_markdown(pattern, reason, gate),
            path: Some(format!("/wiki/drafts/patterns/{slug}")),
            topic: Some(format!("pattern-review-{slug}")),
            summary: Some(format!(
                "Review pattern promotion candidate: {}",
                pattern.summary
            )),
            category: "experience".to_string(),
            keywords: vec![
                "pattern".to_string(),
                "continuity".to_string(),
                "promotion".to_string(),
            ],
            entities: vec!["Tachi".to_string()],
            importance: 0.8,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: Some("continuity".to_string()),
            project: trim_opt(&params.project),
            metadata: Some(json!({
                "review_status": "pending",
                "pattern_promotion": {
                    "pattern_ref": pattern_ref_json(pattern),
                    "reason": reason,
                    "gate": gate,
                    "auto_promote": false,
                }
            })),
            force: true,
            references: Vec::new(),
            include_patterns: true,
            pattern_query: pattern
                .metadata
                .get("projection_key")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some(pattern.id.clone())),
            pattern_top_k: Some(5),
        },
    )
    .await?;
    serde_json::from_str::<Value>(&response).map_err(|e| format!("parse wiki draft response: {e}"))
}

async fn create_skill_candidate(
    server: &MemoryServer,
    params: &TachiEventParams,
    pattern: &MemoryEntry,
    slug: &str,
) -> Result<Value, String> {
    let response = crate::hub_ops::handle_skill_from_pattern(
        server,
        &TachiSkillParams {
            action: "from_pattern".to_string(),
            query: Some(pattern.id.clone()),
            cap_type: None,
            enabled_only: None,
            limit: Some(20),
            skill_id: None,
            args: Some(json!({
                "pattern_ref": pattern.id,
                "skill_id": format!("skill:pattern-{slug}"),
                "name": format!("Pattern: {}", pattern.summary),
                "description": format!("Reviewable skill candidate derived from {}", pattern.path),
                "project": trim_opt(&params.project),
            })),
            profile: None,
            host: None,
            skill_limit: None,
            capability_limit: None,
            include_section: None,
        },
    )
    .await?;
    serde_json::from_str::<Value>(&response)
        .map_err(|e| format!("parse skill candidate response: {e}"))
}

fn emit_agent_profile_proposal(
    server: &MemoryServer,
    params: &TachiEventParams,
    pattern: &MemoryEntry,
    reason: &str,
    gate: &Value,
) -> Result<Value, String> {
    let target = ContinuityEventTarget::from_default_write(server, params.project.as_deref());
    let event = TachiEventRecord {
        id: format!("event-{}", uuid::Uuid::new_v4()),
        source_repo: "tachi".to_string(),
        adapter: "tachi_event.promote".to_string(),
        project: target.project_label(params.project.as_deref()),
        domain: "agent_profile".to_string(),
        session_id: pattern.id.clone(),
        actor: "tachi_event".to_string(),
        event_type: "agent_profile.proposal".to_string(),
        authority: AuthorityLevel::Advisory,
        effects: vec![EffectScope::Prompt],
        projection_hints: vec![ProjectionKind::DomainProfile],
        payload: json!({
            "projection_key": format!("agent-profile:{}", pattern.id),
            "summary": format!("Agent profile proposal from pattern: {}", pattern.summary),
            "text": format!("Review whether this continuity pattern should become an AgentProfile rule: {}", pattern.summary),
            "pattern_ref": pattern_ref_json(pattern),
            "reason": reason,
            "gate": gate,
            "review_status": "pending",
            "write": false,
        }),
        provenance: json!({
            "source": "tachi_event.promote",
            "note": "pattern maturity created a reviewed agent-profile proposal; no host file was written",
        }),
        created_at: now_rfc3339(),
    };
    write_event(server, &target, &event)?;
    Ok(json!({
        "status": "saved",
        "event_id": event.id,
        "event_type": event.event_type,
        "projection_hints": event.projection_hints.iter().map(|projection| projection.as_str()).collect::<Vec<_>>(),
    }))
}

pub(crate) async fn promote_pattern_review_artifacts(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> Result<Value, String> {
    let pattern_ref = pattern_ref_from_params(params)
        .ok_or_else(|| "id or payload.pattern_ref is required when action='promote'".to_string())?;
    let pattern = resolve_pattern(server, params.project.as_deref(), &pattern_ref)?;
    let force = payload_bool(params.payload.as_ref(), "force");
    let reason = promotion_reason(&pattern);
    if reason.is_none() && !force {
        return Err(format!(
            "pattern '{}' is not mature enough to promote; need seen>=3 and hit>0, tier=pattern, or payload.force=true",
            pattern.id
        ));
    }
    let reason = reason.unwrap_or("forced_review");
    let gate = promotion_gate(&pattern);
    let slug = promotion_slug(&pattern);
    let wiki_enabled = !payload_bool(params.payload.as_ref(), "skip_wiki_draft");
    let skill_enabled = !payload_bool(params.payload.as_ref(), "skip_skill_candidate");
    let profile_enabled = !payload_bool(params.payload.as_ref(), "skip_agent_profile_proposal");
    let planned = json!({
        "wiki_draft": wiki_enabled,
        "skill_candidate": skill_enabled,
        "agent_profile_proposal": profile_enabled,
    });
    if params.dry_run {
        return Ok(json!({
            "status": "planned",
            "dry_run": true,
            "pattern_ref": pattern_ref_json(&pattern),
            "reason": reason,
            "gate": gate,
            "planned": planned,
        }));
    }

    let wiki_draft = if wiki_enabled {
        create_wiki_draft(server, params, &pattern, reason, &gate, &slug)
            .await
            .map(Some)?
    } else {
        None
    };
    let skill_candidate = if skill_enabled {
        create_skill_candidate(server, params, &pattern, &slug)
            .await
            .map(Some)?
    } else {
        None
    };
    let agent_profile_proposal = if profile_enabled {
        emit_agent_profile_proposal(server, params, &pattern, reason, &gate).map(Some)?
    } else {
        None
    };

    Ok(json!({
        "status": "completed",
        "action": "promote",
        "auto_promote": false,
        "pattern_ref": pattern_ref_json(&pattern),
        "reason": reason,
        "gate": gate,
        "wiki_draft": wiki_draft.unwrap_or(Value::Null),
        "skill_candidate": skill_candidate.unwrap_or(Value::Null),
        "agent_profile_proposal": agent_profile_proposal.unwrap_or(Value::Null),
    }))
}
