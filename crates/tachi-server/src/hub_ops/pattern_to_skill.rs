use crate::tool_params::TachiSkillParams;
use crate::MemoryServer;
use memcore::{HubCapability, MemoryEntry};
use serde_json::{json, Value};

fn arg_string<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut previous_sep = false;
    for ch in input.trim().chars() {
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
    out.trim_matches('-').to_string()
}

fn pattern_ref_matches(entry: &MemoryEntry, pattern_ref: &str) -> bool {
    entry.id == pattern_ref
        || entry.path == pattern_ref
        || entry.metadata.get("projection_key").and_then(Value::as_str) == Some(pattern_ref)
}

fn pattern_reference(entry: &MemoryEntry) -> Value {
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
        "projection_kind": entry.metadata.get("projection_kind").cloned().unwrap_or(Value::Null),
        "projection_key": entry.metadata.get("projection_key").cloned().unwrap_or(Value::Null),
        "source_event_id": entry.metadata.get("source_event_id").cloned().unwrap_or(Value::Null),
        "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
    })
}

fn resolve_pattern(
    server: &MemoryServer,
    params: &TachiSkillParams,
    args: &Value,
    project: Option<&str>,
) -> Result<MemoryEntry, String> {
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let pattern_ref = params
        .skill_id
        .as_deref()
        .or_else(|| arg_string(args, &["pattern_id", "pattern_path", "pattern_ref"]));
    let query = params
        .query
        .as_deref()
        .or(pattern_ref)
        .filter(|value| !value.trim().is_empty());
    let patterns = crate::continuity_ops::list_active_patterns(server, project, query, limit)?;
    if let Some(pattern_ref) = pattern_ref {
        if let Some(entry) = patterns
            .iter()
            .find(|entry| pattern_ref_matches(entry, pattern_ref))
            .cloned()
        {
            return Ok(entry);
        }
    }
    patterns
        .into_iter()
        .next()
        .ok_or_else(|| "No active pattern projection matched from_pattern input".to_string())
}

fn skill_name(args: &Value, pattern: &MemoryEntry) -> String {
    arg_string(args, &["name"])
        .map(str::to_string)
        .unwrap_or_else(|| format!("Pattern: {}", pattern.summary))
        .chars()
        .take(96)
        .collect()
}

fn skill_id(args: &Value, pattern: &MemoryEntry, name: &str) -> String {
    arg_string(args, &["skill_id", "id"])
        .map(str::to_string)
        .unwrap_or_else(|| {
            let key = pattern
                .metadata
                .get("projection_key")
                .and_then(Value::as_str)
                .unwrap_or(name);
            let slug = slugify(key);
            format!(
                "skill:pattern-{}",
                if slug.is_empty() {
                    crate::utils::stable_hash(&pattern.id)
                } else {
                    slug
                }
            )
        })
}

fn skill_definition(pattern: &MemoryEntry, pattern_ref: Value) -> Result<String, String> {
    let prompt = format!(
        "Task: {{{{task}}}}\n\nContext: {{{{context}}}}\n\nContinuity pattern:\n{}\n\nReturn concrete steps, evidence to check, and open risks. Do not execute external actions unless the caller explicitly asks.",
        pattern.text
    );
    serde_json::to_string(&json!({
        "kind": "pattern_derived_skill",
        "system": "Apply the referenced continuity pattern as a reviewable workflow aid. Use it as guidance, not as hidden authority.",
        "prompt": prompt,
        "inputSchema": {
            "type": "object",
            "required": ["task"],
            "properties": {
                "task": {"type": "string"},
                "context": {"type": "string"}
            }
        },
        "policy": {
            "visibility": "discoverable",
            "requires_review": true,
            "source": "continuity_pattern"
        },
        "tags": ["pattern-derived", "continuity", "memory"],
        "pattern_ref": pattern_ref,
    }))
    .map_err(|e| format!("serialize pattern skill definition: {e}"))
}

pub(crate) async fn handle_skill_from_pattern(
    server: &MemoryServer,
    params: &TachiSkillParams,
) -> Result<String, String> {
    let args = params.args.clone().unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return Err("args must be a JSON object when action='from_pattern'".to_string());
    }
    let project = arg_string(&args, &["project"]);
    let pattern = resolve_pattern(server, params, &args, project)?;
    let name = skill_name(&args, &pattern);
    let id = skill_id(&args, &pattern, &name);
    let pattern_ref = pattern_reference(&pattern);
    let description = arg_string(&args, &["description"])
        .map(str::to_string)
        .unwrap_or_else(|| format!("Reviewable skill candidate derived from {}", pattern.path));
    let definition = skill_definition(&pattern, pattern_ref.clone())?;
    let cap = HubCapability {
        id: id.clone(),
        cap_type: "skill".to_string(),
        name: name.clone(),
        version: 1,
        description: description.clone(),
        definition: definition.clone(),
        enabled: false,
        review_status: "pending".to_string(),
        health_status: "unknown".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

    if let Some(project) = project {
        server.with_named_project_store(project, |store| {
            store
                .hub_register(&cap)
                .map_err(|e| format!("register pattern skill candidate: {e}"))
        })?;
    } else {
        let (target_db, _) = server.resolve_write_scope(arg_string(&args, &["scope"]).unwrap_or(
            if server.has_project_db() {
                "project"
            } else {
                "global"
            },
        ));
        server.with_store_for_scope(target_db, |store| {
            store
                .hub_register(&cap)
                .map_err(|e| format!("register pattern skill candidate: {e}"))
        })?;
    }

    Ok(json!({
        "action": "from_pattern",
        "status": "candidate_registered",
        "id": id,
        "name": name,
        "description": description,
        "enabled": false,
        "review_status": "pending",
        "callable": false,
        "visibility": "discoverable",
        "pattern_ref": pattern_ref,
        "definition": serde_json::from_str::<Value>(&definition).unwrap_or(Value::Null),
    })
    .to_string())
}
