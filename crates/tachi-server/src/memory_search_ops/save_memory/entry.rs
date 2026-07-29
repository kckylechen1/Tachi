use crate::tool_params::SaveMemoryParams;
use crate::{DbScope, MemoryServer};
use memcore::MemoryEntry;
use serde_json::json;
use tachi_llm::PersistedModelInvocationReceiptV1;

pub(in crate::memory_search_ops::save_memory) fn build_save_entry(
    server: &MemoryServer,
    params: SaveMemoryParams,
    safe_text: String,
    id: String,
    timestamp: String,
    valid_from: String,
    target_db: DbScope,
    existing: Option<&MemoryEntry>,
    model_invocation: Option<&PersistedModelInvocationReceiptV1>,
) -> Result<MemoryEntry, String> {
    let is_patch = params.id.is_some() && existing.is_some();
    let requested_scope = params.scope;
    let path = patch_string_field(
        is_patch,
        existing.map(|entry| entry.path.as_str()),
        params.path,
        |value| value.trim().is_empty() || value.trim() == "/",
    );
    let category = patch_string_field(
        is_patch,
        existing.map(|entry| entry.category.as_str()),
        params.category,
        |value| value.trim().is_empty() || value.trim() == "fact",
    );
    let topic = patch_string_field(
        is_patch,
        existing.map(|entry| entry.topic.as_str()),
        params.topic,
        |value| value.trim().is_empty(),
    );
    let summary = patch_string_field(
        is_patch,
        existing.map(|entry| entry.summary.as_str()),
        params.summary,
        |value| value.trim().is_empty(),
    );
    let importance = if is_patch && (params.importance - 0.7).abs() < f64::EPSILON {
        existing
            .map(|entry| entry.importance)
            .unwrap_or(params.importance)
    } else {
        params.importance
    };
    let requested_domain = params.domain;
    let domain = if is_patch && requested_domain.is_none() {
        existing.and_then(|entry| entry.domain.clone())
    } else {
        resolve_save_domain(requested_domain, &path, &category)
    };
    let requested_retention = params.retention_policy;
    let retention_policy = if is_patch && requested_retention.is_none() {
        existing.and_then(|entry| entry.retention_policy.clone())
    } else {
        resolve_save_retention_policy(requested_retention, &path, &category)
    };
    let requested_valid_from = params.valid_from.clone();
    let final_valid_from = if is_patch
        && requested_valid_from
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        existing
            .map(|entry| entry.valid_from.clone())
            .unwrap_or(valid_from)
    } else {
        valid_from
    };
    let valid_until = if is_patch
        && params
            .valid_until
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        existing.and_then(|entry| entry.valid_until.clone())
    } else {
        params.valid_until.clone()
    };
    let mut incoming_metadata = params.metadata.unwrap_or_else(|| json!({}));
    if is_patch {
        incoming_metadata =
            merge_patch_metadata(existing.map(|entry| &entry.metadata), incoming_metadata);
    }
    let mut metadata = crate::provenance::inject_provenance(
        server,
        incoming_metadata,
        "save_memory",
        "memory_write",
        Some(requested_scope.as_str()),
        target_db,
        json!({
            "path": path,
            "category": category,
            "topic": topic,
        }),
    );
    // A typed receipt belongs to the artifact's first durable write. Preserve
    // an existing winner exactly as stored rather than turning a replay or
    // wiki update into a provenance overwrite.
    if existing.is_none() {
        if let Some(invocation) = model_invocation {
            metadata = crate::provenance::attach_model_invocation(metadata, invocation)?;
        }
    }
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("force".to_string(), serde_json::Value::Bool(params.force));
        if !params.location.trim().is_empty() {
            obj.insert(
                "legacy_location".to_string(),
                serde_json::Value::String(params.location.trim().to_string()),
            );
        }
    }
    let tier = metadata
        .get("tier")
        .and_then(serde_json::Value::as_str)
        .filter(|value| matches!(*value, "raw" | "consolidated" | "pattern"))
        .unwrap_or("raw")
        .to_string();

    let mut entities = if is_patch && params.entities.is_empty() && params.persons.is_empty() {
        existing
            .map(|entry| entry.entities.clone())
            .unwrap_or_default()
    } else {
        params.entities
    };
    memcore::types::fold_person_names_into_entities(&mut entities, params.persons);
    let keywords = if is_patch && params.keywords.is_empty() {
        existing
            .map(|entry| entry.keywords.clone())
            .unwrap_or_default()
    } else {
        params.keywords
    };

    Ok(MemoryEntry {
        id,
        path,
        summary,
        text: safe_text,
        importance: importance.clamp(0.0, 1.0),
        timestamp,
        valid_from: final_valid_from,
        valid_until,
        category,
        topic,
        keywords,
        persons: Vec::new(),
        entities,
        location: String::new(),
        source: "mcp".to_string(),
        scope: requested_scope,
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata,
        vector: params.vector,
        retention_policy,
        domain,
        recall_count: 0,
        query_diversity: 0,
        tier,
    })
}

fn patch_string_field(
    is_patch: bool,
    existing: Option<&str>,
    incoming: String,
    is_default_or_empty: impl Fn(&str) -> bool,
) -> String {
    if is_patch && is_default_or_empty(&incoming) {
        existing.map(str::to_string).unwrap_or(incoming)
    } else {
        incoming
    }
}

fn merge_patch_metadata(
    existing: Option<&serde_json::Value>,
    incoming: serde_json::Value,
) -> serde_json::Value {
    let mut merged = existing
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let serde_json::Value::Object(incoming_obj) = incoming {
        for (key, value) in incoming_obj {
            merged.insert(key, value);
        }
    }
    serde_json::Value::Object(merged)
}

/// `pub(super)`: also used by `write_affinity`'s pre-write domain
/// classification (#1041 S1) so the affinity gate checks the SAME resolved
/// domain that will end up on the entry, instead of re-deriving its own
/// classification.
pub(super) fn resolve_save_domain(
    requested_domain: Option<String>,
    path: &str,
    category: &str,
) -> Option<String> {
    if let Some(target) =
        crate::repair::domain::repair_target(requested_domain.as_deref(), path, category, "mcp")
    {
        return Some(target);
    }
    requested_domain
        .map(|domain| domain.trim().to_string())
        .filter(|domain| !domain.is_empty())
}

fn resolve_save_retention_policy(
    requested_retention: Option<String>,
    path: &str,
    category: &str,
) -> Option<String> {
    if let Some(retention) = requested_retention
        .map(|retention| retention.trim().to_ascii_lowercase())
        .filter(|retention| !retention.is_empty())
    {
        return Some(retention);
    }
    Some(crate::repair::retention::default_retention_for_row(path, category, "mcp").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_save_domain_matches_r9_repair_targets() {
        assert_eq!(
            resolve_save_domain(None, "/scratch/sigil/fix", "decision"),
            Some("scratch".to_string())
        );
        assert_eq!(
            resolve_save_domain(Some("ProjectAlpha".to_string()), "/project/example", "fact"),
            Some("projectalpha".to_string())
        );
        assert_eq!(
            resolve_save_domain(Some("/notes/raw".to_string()), "/notes/today", "note"),
            Some("notes".to_string())
        );
        assert_eq!(
            resolve_save_domain(Some("coding".to_string()), "/scratch/sigil", "fact"),
            Some("coding".to_string())
        );
    }

    #[test]
    fn resolve_save_retention_policy_matches_r2_backfill_targets() {
        assert_eq!(
            resolve_save_retention_policy(None, "/scratch/sigil/fix", "fact"),
            Some("durable".to_string())
        );
        assert_eq!(
            resolve_save_retention_policy(None, "/scratch/sigil/decision", "decision"),
            Some("permanent".to_string())
        );
        assert_eq!(
            resolve_save_retention_policy(None, "/handoff/worker", "fact"),
            Some("pinned".to_string())
        );
        assert_eq!(
            resolve_save_retention_policy(None, "/ghost/run", "fact"),
            Some("ephemeral".to_string())
        );
        assert_eq!(
            resolve_save_retention_policy(Some("Permanent".to_string()), "/scratch", "fact"),
            Some("permanent".to_string())
        );
    }
}
