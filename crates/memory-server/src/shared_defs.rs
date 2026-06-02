use super::*;

pub(super) const DLQ_MAX_ENTRIES: usize = 200;
pub(super) const DLQ_TTL_SECS: u64 = 3600;

#[derive(Clone)]
pub(super) struct DeadLetter {
    pub(super) id: String,
    pub(super) tool_name: String,
    pub(super) arguments: Option<serde_json::Map<String, serde_json::Value>>,
    pub(super) error: String,
    pub(super) error_category: String,
    pub(super) timestamp: String,
    pub(super) retry_count: u32,
    pub(super) max_retries: u32,
    pub(super) status: String,
}

pub(super) fn categorize_error(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("not found") || lower.contains("not_found") {
        "not_found".to_string()
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "timeout".to_string()
    } else if lower.contains("invalid") || lower.contains("param") {
        "invalid_params".to_string()
    } else {
        "internal".to_string()
    }
}

pub(super) fn slim_entry(e: &MemoryEntry, db: DbScope) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(e.id));
    obj.insert("db".into(), json!(db.as_str()));
    obj.insert("text".into(), json!(e.text));
    if !e.summary.is_empty() {
        obj.insert("summary".into(), json!(e.summary));
    }
    obj.insert("path".into(), json!(e.path));
    if !e.topic.is_empty() {
        obj.insert("topic".into(), json!(e.topic));
    }
    if !e.keywords.is_empty() {
        obj.insert("keywords".into(), json!(e.keywords));
    }
    obj.insert("importance".into(), json!(e.importance));
    obj.insert("timestamp".into(), json!(e.timestamp));
    obj.insert("category".into(), json!(e.category));
    obj.insert("scope".into(), json!(e.scope));
    if !e.persons.is_empty() {
        obj.insert("persons".into(), json!(e.persons));
    }
    if !e.entities.is_empty() {
        obj.insert("entities".into(), json!(e.entities));
    }
    if !e.location.is_empty() {
        obj.insert("location".into(), json!(e.location));
    }
    if let Some(ref rp) = e.retention_policy {
        obj.insert("retention_policy".into(), json!(rp));
    }
    if let Some(ref domain) = e.domain {
        if !domain.is_empty() {
            obj.insert("domain".into(), json!(domain));
        }
    }
    if e.archived {
        obj.insert("archived".into(), json!(true));
    }
    if let serde_json::Value::Object(ref m) = e.metadata {
        if !m.is_empty() {
            obj.insert("metadata".into(), json!(m));
        }
    }
    serde_json::Value::Object(obj)
}

/// Like `slim_entry` but additionally surfaces enrichment status fields:
///   - `embedding_pending`: true when no vector has been written yet
///   - `summary_pending`:   true when no summary has been written yet
///   - `foundry_jobs`:      array of `{id, kind, status, created_at}` for any
///     foundry jobs currently touching this memory id (queued/running first,
///     omitted when empty)
///
/// Used by `get_memory` so agents can tell whether async post-write enrichment
/// is still in flight versus complete. Intentionally NOT used by `list_memories`
/// or search results to avoid an N+1 lookup against `foundry_jobs`.
pub(super) fn slim_entry_with_enrichment(
    server: &MemoryServer,
    e: &MemoryEntry,
    db: DbScope,
    named_project: Option<&str>,
) -> serde_json::Value {
    let mut obj = match slim_entry(e, db) {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };

    obj.insert("embedding_pending".into(), json!(e.vector.is_none()));
    obj.insert("summary_pending".into(), json!(e.summary.is_empty()));

    // Foundry job lookup runs against the same store the entry came from. Any
    // failure (table missing, transient lock, …) is non-fatal — we simply omit
    // the field so callers can rely on the entry payload itself.
    let lookup = |store: &mut MemoryStore| -> Result<Vec<memory_core::FoundryJobSummary>, String> {
        memory_core::find_foundry_jobs_for_memory(store.connection(), &e.id)
            .map_err(|err| err.to_string())
    };

    let jobs_res = match (db, named_project) {
        (DbScope::Project, Some(name)) => server.with_named_project_store_read(name, lookup),
        (DbScope::Project, None) => server.with_project_store_read(lookup),
        (DbScope::Global, _) => server.with_global_store_read(lookup),
    };

    if let Ok(jobs) = jobs_res {
        if !jobs.is_empty() {
            obj.insert("foundry_jobs".into(), json!(jobs));
        }
    }

    serde_json::Value::Object(obj)
}

fn round_score(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Token-efficient search hit: id/path/topic/summary plus scores only.
/// Full text and metadata remain available via `get_memory`.
///
/// Pass `include_metadata = true` to also surface the full `metadata` map.
/// This is required for callers that need to read semantic fields like
/// kanban `a2a_state`; for everything else the default keeps token usage
/// tight by omitting the metadata blob.
pub(super) fn slim_search_result(
    result: &memory_core::SearchResult,
    db: DbScope,
    include_metadata: bool,
) -> serde_json::Value {
    let entry = &result.entry;
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(entry.id));
    obj.insert("db".into(), json!(db.as_str()));
    obj.insert("path".into(), json!(entry.path));
    if !entry.topic.is_empty() {
        obj.insert("topic".into(), json!(entry.topic));
    }
    if !entry.summary.is_empty() {
        obj.insert("summary".into(), json!(entry.summary));
    }
    if include_metadata {
        // Always include metadata when requested — consumers like the kanban
        // board rely on it to surface a2a_state, eval_ledger_id, agent, etc.
        obj.insert("metadata".into(), entry.metadata.clone());
    } else {
        // Surface referenced source files (metadata.files) inline so agents can
        // jump to the file without a follow-up get_memory. Only string entries
        // are kept.
        if let Some(serde_json::Value::Array(files)) = entry.metadata.get("files") {
            let paths: Vec<&str> = files.iter().filter_map(|v| v.as_str()).collect();
            if !paths.is_empty() {
                obj.insert("files".into(), json!(paths));
            }
        }
    }
    obj.insert(
        "relevance".into(),
        json!(round_score(result.score.final_score)),
    );
    obj.insert(
        "score".into(),
        json!({
            "vector": round_score(result.score.vector),
            "fts": round_score(result.score.fts),
            "symbolic": round_score(result.score.symbolic),
            "decay": round_score(result.score.decay),
            "final": round_score(result.score.final_score),
        }),
    );
    serde_json::Value::Object(obj)
}

pub(super) fn slim_l0_rule(rule: &MemoryEntry, db: DbScope) -> serde_json::Value {
    let summary = if !rule.summary.is_empty() {
        rule.summary.clone()
    } else {
        rule.text.chars().take(200).collect()
    };
    json!({
        "id": rule.id,
        "db": db.as_str(),
        "path": rule.path,
        "topic": if rule.topic.is_empty() { serde_json::Value::Null } else { json!(rule.topic) },
        "summary": summary,
        "l0_rule": true,
    })
}
