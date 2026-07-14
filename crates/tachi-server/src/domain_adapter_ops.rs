use crate::tool_params::{TachiDomainAdapterParams, TachiEventParams};
use crate::MemoryServer;
use memory_server_runtime::trim_opt;
use serde_json::{json, Map, Value};

fn json_string(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("serialize domain adapter response: {e}"))
}

fn string_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn string_vec_field(value: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        if let Some(raw) = value.get(*key) {
            match raw {
                Value::Array(items) => {
                    return items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .collect();
                }
                Value::String(item) if !item.trim().is_empty() => {
                    return vec![item.trim().to_string()];
                }
                _ => {}
            }
        }
    }
    Vec::new()
}

fn lorebook_payload(entry: &Value, character: Option<&str>) -> Result<Value, String> {
    let content = string_field(entry, &["content", "text"])
        .ok_or_else(|| "lorebook entry content is required".to_string())?;
    let keys = string_vec_field(entry, &["keys"]);
    let secondary_keys = string_vec_field(entry, &["secondary_keys", "secondaryKeys"]);
    let mut payload = Map::new();
    if let Some(id) = string_field(entry, &["id", "uid"]) {
        payload.insert("entry_id".to_string(), json!(id));
    }
    if let Some(character) = character {
        payload.insert("character".to_string(), json!(character));
    }
    payload.insert(
        "lorebook_key".to_string(),
        json!(string_field(entry, &["id", "uid"])
            .or_else(|| keys.first().map(String::as_str))
            .unwrap_or(content)),
    );
    payload.insert(
        "summary".to_string(),
        json!(string_field(entry, &["summary"])
            .or_else(|| keys.first().map(String::as_str))
            .unwrap_or("lorebook entry")),
    );
    payload.insert("content".to_string(), json!(content));
    payload.insert("keys".to_string(), json!(keys));
    if !secondary_keys.is_empty() {
        payload.insert("secondary_keys".to_string(), json!(secondary_keys));
    }
    for key in [
        "position",
        "priority",
        "constant",
        "selective",
        "recursive",
        "enabled",
    ] {
        if let Some(value) = entry.get(key) {
            payload.insert(key.to_string(), value.clone());
        }
    }
    if let Some(value) = entry
        .get("token_budget")
        .or_else(|| entry.get("tokenBudget"))
    {
        payload.insert("token_budget".to_string(), value.clone());
    }
    payload
        .entry("enabled".to_string())
        .or_insert_with(|| json!(true));
    payload
        .entry("position".to_string())
        .or_insert_with(|| json!("before_char"));
    payload
        .entry("constant".to_string())
        .or_insert_with(|| json!(false));
    payload
        .entry("selective".to_string())
        .or_insert_with(|| json!(false));
    payload
        .entry("recursive".to_string())
        .or_insert_with(|| json!(false));
    Ok(Value::Object(payload))
}

async fn lorebook_import(
    server: &MemoryServer,
    params: TachiDomainAdapterParams,
) -> Result<String, String> {
    if params.entries.is_empty() {
        return Err("entries is required when action='lorebook_import'".to_string());
    }
    let project = trim_opt(&params.project);
    let domain = trim_opt(&params.domain).unwrap_or_else(|| "lorebook".to_string());
    let actor = trim_opt(&params.actor).unwrap_or_else(|| "domain_adapter".to_string());
    let session_id = trim_opt(&params.session_id).unwrap_or_else(|| {
        format!(
            "lorebook-import:{}",
            params.character.as_deref().unwrap_or("unscoped")
        )
    });
    let character = trim_opt(&params.character);
    let mut imported = Vec::new();
    for (idx, entry) in params.entries.iter().enumerate() {
        let payload = lorebook_payload(entry, character.as_deref())?;
        let key = payload
            .get("lorebook_key")
            .and_then(Value::as_str)
            .unwrap_or("entry");
        let event_id = format!(
            "lorebook:{}:{}:{}",
            character.as_deref().unwrap_or("global"),
            idx,
            crate::utils::stable_hash(&format!("{key}|{}", payload))
        );
        let event_params = TachiEventParams {
            action: "emit".to_string(),
            format: Some("json".to_string()),
            id: Some(event_id.clone()),
            source_repo: Some("romanbath".to_string()),
            adapter: Some("domain_adapter:lorebook".to_string()),
            project: project.clone(),
            // #1114 (codex round-1 B1 fix): CARRY the caller's own
            // `project_explicit` marker through rather than re-deriving it
            // from `project.is_some()` — `params.project` here may be a
            // transport-injected session default (bound session,
            // `tachi_domain_adapter` omitted `project=`), and
            // `project.is_some()` alone cannot distinguish that from a
            // genuine caller `project=`. Re-deriving silently turned every
            // transport default into a false "explicit", bypassing this
            // event's own write-affinity scrutiny in `handle_tachi_event`.
            project_explicit: params.project_explicit,
            domain: Some(domain.clone()),
            session_id: Some(session_id.clone()),
            actor: Some(actor.clone()),
            event_type: Some("lorebook.entry.upsert".to_string()),
            authority: Some("collect_only".to_string()),
            effects: vec!["prompt".to_string()],
            projection_hints: vec!["world_book".to_string()],
            payload: Some(payload.clone()),
            provenance: Some(json!({
                "adapter_pack": "lorebook_worldbook",
                "character": character,
                "entry_index": idx,
            })),
            created_at: None,
            limit: 20,
            path_prefix: None,
            dry_run: params.dry_run,
        };
        if params.dry_run {
            imported.push(json!({
                "event_id": event_id,
                "payload": payload,
            }));
        } else {
            let body = crate::event_ops::handle_tachi_event(server, event_params).await?;
            let saved: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body}));
            imported.push(saved);
        }
    }

    let projected = if params.project_events && !params.dry_run {
        let project_params = TachiEventParams {
            action: "project".to_string(),
            format: Some("json".to_string()),
            id: None,
            source_repo: None,
            adapter: None,
            // #1114 (codex round-1 B1 fix): see the `emit` construction
            // above — carry the caller's actual marker, don't re-derive it.
            project_explicit: params.project_explicit,
            project,
            domain: Some(domain),
            session_id: None,
            actor: None,
            event_type: None,
            authority: None,
            effects: Vec::new(),
            projection_hints: vec!["world_book".to_string()],
            payload: None,
            provenance: None,
            created_at: None,
            limit: params.entries.len().clamp(1, 500),
            path_prefix: None,
            dry_run: false,
        };
        let body = crate::event_ops::handle_tachi_event(server, project_params).await?;
        Some(serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body})))
    } else {
        None
    };

    json_string(&json!({
        "status": if params.dry_run { "dry_run" } else { "completed" },
        "adapter": "lorebook_worldbook",
        "imported_count": imported.len(),
        "events": imported,
        "projection": projected,
    }))
}

pub(crate) async fn handle_tachi_domain_adapter(
    server: &MemoryServer,
    params: TachiDomainAdapterParams,
) -> Result<String, String> {
    match params.action.trim().to_ascii_lowercase().as_str() {
        "lorebook_import" | "worldbook_import" => lorebook_import(server, params).await,
        other => Err(format!(
            "invalid domain adapter action: {other}. Use lorebook_import."
        )),
    }
}
