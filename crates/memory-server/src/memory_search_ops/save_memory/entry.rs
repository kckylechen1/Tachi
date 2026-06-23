use crate::tool_params::SaveMemoryParams;
use crate::{DbScope, MemoryServer};
use memory_core::MemoryEntry;
use serde_json::json;

pub(in crate::memory_search_ops::save_memory) fn build_save_entry(
    server: &MemoryServer,
    params: SaveMemoryParams,
    safe_text: String,
    id: String,
    timestamp: String,
    valid_from: String,
    target_db: DbScope,
) -> MemoryEntry {
    let requested_scope = params.scope;
    let path = params.path;
    let category = params.category;
    let topic = params.topic;
    let mut metadata = crate::provenance::inject_provenance(
        server,
        params.metadata.unwrap_or_else(|| json!({})),
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

    let mut entities = params.entities;
    memory_core::types::fold_person_names_into_entities(&mut entities, params.persons);

    MemoryEntry {
        id,
        path,
        summary: params.summary,
        text: safe_text,
        importance: params.importance.clamp(0.0, 1.0),
        timestamp,
        valid_from,
        valid_until: params.valid_until,
        category,
        topic,
        keywords: params.keywords,
        persons: Vec::new(),
        entities,
        location: String::new(),
        source: "mcp".to_string(),
        scope: requested_scope,
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: params.vector,
        retention_policy: params.retention_policy,
        domain: params.domain,
        recall_count: 0,
        query_diversity: 0,
        tier,
    }
}
