use chrono::{DateTime, SecondsFormat, Utc};
use memory_core::{AuthorityLevel, EffectScope, ProjectionKind, TachiEventQuery, TachiEventRecord};
use memory_server_runtime::query_limit;
use serde_json::json;

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;

fn json_string(value: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("serialize tachi_event response: {e}"))
}

fn trim_string(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn normalize_created_at(raw: Option<String>) -> Result<String, String> {
    let Some(raw) = trim_string(raw) else {
        return Ok(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true));
    };
    DateTime::parse_from_rfc3339(&raw)
        .map(|dt| {
            dt.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
        .map_err(|_| format!("invalid created_at timestamp: {raw}"))
}

fn parse_authority(raw: Option<&str>) -> Result<AuthorityLevel, String> {
    let normalized = raw.unwrap_or("").trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "collect_only" => Ok(AuthorityLevel::CollectOnly),
        "review_signal_only" => Ok(AuthorityLevel::ReviewSignalOnly),
        "interaction_routing_only" => Ok(AuthorityLevel::InteractionRoutingOnly),
        "tone_and_reminder_only" => Ok(AuthorityLevel::ToneAndReminderOnly),
        "advisory" => Ok(AuthorityLevel::Advisory),
        "raw_fact" => Ok(AuthorityLevel::RawFact),
        "derived_evidence" => Ok(AuthorityLevel::DerivedEvidence),
        "blocker" => Ok(AuthorityLevel::Blocker),
        "execution_gate" => Ok(AuthorityLevel::ExecutionGate),
        _ => Err(format!("invalid authority level: {normalized}")),
    }
}

fn parse_effect(raw: &str) -> Result<EffectScope, String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok(EffectScope::None);
    }
    let effect = EffectScope::from_str_opt(Some(&normalized));
    if effect == EffectScope::None && normalized != "none" {
        return Err(format!("invalid effect scope: {normalized}"));
    }
    Ok(effect)
}

fn parse_projection(raw: &str) -> Result<ProjectionKind, String> {
    let normalized = raw.trim().to_ascii_lowercase();
    ProjectionKind::from_str_opt(Some(&normalized))
        .ok_or_else(|| format!("invalid projection hint: {normalized}"))
}

fn parse_effects(values: &[String]) -> Result<Vec<EffectScope>, String> {
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(parse_effect(trimmed)?);
    }
    Ok(out)
}

fn parse_projection_hints(values: &[String]) -> Result<Vec<ProjectionKind>, String> {
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(parse_projection(trimmed)?);
    }
    Ok(out)
}

fn workspace_project_label() -> Option<String> {
    crate::memory_search_ops::resolve_workspace_named_project()
}

fn event_project_label(params_project: &Option<String>) -> String {
    trim_string(params_project.clone())
        .or_else(workspace_project_label)
        .unwrap_or_default()
}

fn emit_event(server: &MemoryServer, params: TachiEventParams) -> Result<String, String> {
    let event_type = trim_string(params.event_type.clone())
        .ok_or_else(|| "event_type is required when action='emit'".to_string())?;
    let event = TachiEventRecord {
        id: trim_string(params.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        source_repo: trim_string(params.source_repo.clone()).unwrap_or_else(|| "tachi".to_string()),
        adapter: trim_string(params.adapter.clone()).unwrap_or_else(|| "memory-server".to_string()),
        project: event_project_label(&params.project),
        domain: trim_string(params.domain.clone()).unwrap_or_default(),
        session_id: trim_string(params.session_id.clone()).unwrap_or_default(),
        actor: trim_string(params.actor.clone()).unwrap_or_else(|| "agent".to_string()),
        event_type,
        authority: parse_authority(params.authority.as_deref())?,
        effects: parse_effects(&params.effects)?,
        projection_hints: parse_projection_hints(&params.projection_hints)?,
        payload: params.payload.clone().unwrap_or_else(|| json!({})),
        provenance: params.provenance.clone().unwrap_or_else(|| json!({})),
        created_at: normalize_created_at(params.created_at.clone())?,
    };

    let route = server.event_db_route(params.project.as_deref());
    server.with_event_route_store(&route, |store| {
        store
            .insert_tachi_event(&event)
            .map_err(|e| format!("insert tachi event: {e}"))
    })?;

    json_string(&json!({
        "status": "saved",
        "id": event.id,
        "event": event,
    }))
}

fn query_events(server: &MemoryServer, params: TachiEventParams) -> Result<String, String> {
    let query = TachiEventQuery {
        project: trim_string(params.project.clone()),
        domain: trim_string(params.domain.clone()),
        event_type: trim_string(params.event_type.clone()),
        session_id: trim_string(params.session_id.clone()),
        source_repo: trim_string(params.source_repo.clone()),
        adapter: trim_string(params.adapter.clone()),
        limit: query_limit(params.limit),
    };

    let route = server.event_db_route(params.project.as_deref());
    let events = server.with_event_route_store_read(&route, |store| {
        store
            .list_tachi_events(&query)
            .map_err(|e| format!("list tachi events: {e}"))
    })?;

    json_string(&json!({
        "status": "completed",
        "count": events.len(),
        "events": events,
    }))
}

fn event_metrics(server: &MemoryServer, params: TachiEventParams) -> Result<String, String> {
    let limit = query_limit(params.limit);
    let route = server.event_db_route(params.project.as_deref());
    let metrics = server.with_event_route_store_read(&route, |store| {
        store
            .continuity_metrics(limit)
            .map_err(|e| format!("compute continuity metrics: {e}"))
    })?;

    json_string(&json!({
        "status": "completed",
        "metrics": metrics,
    }))
}

pub(crate) async fn handle_tachi_event(
    server: &MemoryServer,
    params: TachiEventParams,
) -> Result<String, String> {
    match params.action.to_ascii_lowercase().as_str() {
        "emit" => emit_event(server, params),
        "query" => query_events(server, params),
        "metrics" => event_metrics(server, params),
        "project" => {
            let value = crate::continuity_ops::project_continuity_events(server, &params)?;
            json_string(&value)
        }
        "promote" => {
            let value = crate::continuity_ops::promote_pattern_review_artifacts(server, &params)
                .await?;
            json_string(&value)
        }
        "context" => {
            let value = crate::continuity_ops::build_continuity_context(server, &params)?;
            json_string(&value)
        }
        "a2a" => {
            let value = crate::continuity_ops::build_a2a_context(server, &params)?;
            json_string(&value)
        }
        "label_eval" => {
            let value = crate::continuity_ops::evaluate_outcome_labels(server, &params)?;
            json_string(&value)
        }
        _ => Err(format!(
            "Invalid action '{}'. Use 'emit', 'query', 'metrics', 'project', 'promote', 'context', 'a2a', or 'label_eval'.",
            params.action
        )),
    }
}
