use super::*;
use crate::memory_search_ops::auto_link::{is_training_seed, spawn_auto_linking};
use crate::memory_search_ops::contradiction::apply_auto_contradiction_detection;
use crate::memory_search_ops::text_scrub::{scrub_secrets, scrub_think_tags};

pub(crate) enum SaveTextValidation {
    Accepted(Option<serde_json::Value>),
    Rejected(String),
}

fn json_response(value: serde_json::Value) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) fn validate_save_text(
    params: &SaveMemoryParams,
    safe_text: &str,
) -> Result<SaveTextValidation, String> {
    if !params.force && memory_core::is_noise_text(safe_text) {
        return Ok(SaveTextValidation::Rejected(json_response(json!({
            "saved": false,
            "noise": true,
            "reason": "Text detected as noise (greeting, denial, or meta-question). Not saved.",
            "hint": "Retry with force=true if this is intentional content.",
        }))?));
    }

    // Capture gate (Branch #4): validate domain, path bucket, min-chars, and
    // markdown-dump heuristic. Default mode = Warn (annotate response, write
    // proceeds). TACHI_CAPTURE_GATE=enforce switches to hard rejection.
    let gate_mode = crate::capture_gate::GateMode::from_env();
    let gate_decision = crate::capture_gate::evaluate(
        &crate::capture_gate::GateInput::new(
            safe_text,
            &params.path,
            params.domain.as_deref(),
            params.force,
        ),
        gate_mode,
    );
    if !gate_decision.accept {
        return Ok(SaveTextValidation::Rejected(json_response(json!({
            "saved": false,
            "rejected_by": "capture_gate",
            "mode": gate_decision.mode,
            "violations": gate_decision.violations,
            "hint": "Set TACHI_CAPTURE_GATE=warn to downgrade these to warnings, or pass force=true on save.",
        }))?));
    }

    Ok(SaveTextValidation::Accepted(
        (!gate_decision.violations.is_empty()).then(|| json!(gate_decision.violations)),
    ))
}

pub(crate) fn lookup_existing_revision(
    server: &MemoryServer,
    id: &str,
    requested_id: bool,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<Option<i64>, String> {
    if !requested_id {
        return Ok(None);
    }

    let lookup = |store: &mut MemoryStore| {
        store
            .get(id)
            .map(|entry| entry.map(|entry| entry.revision))
            .map_err(|e| format_save_error(server, target_db, named_project, &e))
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, lookup)
    } else {
        server.with_store_for_scope_read(target_db, lookup)
    }
}

pub(crate) fn build_save_entry(
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
            obj.entry("legacy_location".to_string())
                .or_insert_with(|| serde_json::Value::String(params.location.trim().to_string()));
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

pub(crate) fn upsert_save_entry(
    server: &MemoryServer,
    entry: &MemoryEntry,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<(), String> {
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, |store: &mut MemoryStore| {
            store
                .upsert(entry)
                .map_err(|e| format_save_error(server, target_db, Some(project_name), &e))
        })
    } else {
        server.with_store_for_scope(target_db, |store: &mut MemoryStore| {
            store
                .upsert(entry)
                .map_err(|e| format_save_error(server, target_db, None, &e))
        })
    }
}

pub(crate) fn spawn_save_contradiction_detection(
    server: &MemoryServer,
    entry_id: String,
    target_db: DbScope,
    named_project: Option<String>,
) {
    let contradiction_server = server.clone();
    tokio::spawn(async move {
        if let Err(err) = apply_auto_contradiction_detection(
            &contradiction_server,
            &entry_id,
            target_db,
            named_project.as_deref(),
            None,
        )
        .await
        {
            eprintln!("[save_memory] auto contradiction detection failed for {entry_id}: {err}");
        }
    });
}

fn should_enqueue_enrichment(_entry: &MemoryEntry) -> bool {
    true
}

fn enrichment_work_pending(
    entry: &MemoryEntry,
    needs_embedding: bool,
    needs_summary: bool,
) -> bool {
    needs_embedding
        || needs_summary
        || crate::enrichment::needs_metadata_enrichment(&entry.keywords, &entry.entities)
}

pub(crate) fn enqueue_save_enrichment(
    server: &MemoryServer,
    entry: &MemoryEntry,
    needs_embedding: bool,
    needs_summary: bool,
    target_db: DbScope,
    named_project: Option<String>,
    enrichment_revision: i64,
) -> bool {
    if !enrichment_work_pending(entry, needs_embedding, needs_summary)
        || !should_enqueue_enrichment(entry)
    {
        return false;
    }

    server.enqueue_enrichment(crate::enrichment::build_enrichment_item(
        entry,
        needs_embedding,
        needs_summary,
        target_db,
        named_project,
        None,
        None,
        None,
        enrichment_revision,
    ));
    true
}

pub(crate) fn build_save_response(
    entry: &MemoryEntry,
    timestamp: &str,
    target_db: DbScope,
    enrichment_enqueued: bool,
    needs_embedding: bool,
    needs_summary: bool,
    warning: Option<String>,
    gate_warnings: Option<serde_json::Value>,
    secret_redactions: usize,
) -> serde_json::Map<String, serde_json::Value> {
    let mut response = serde_json::Map::new();
    response.insert("id".into(), json!(entry.id.clone()));
    response.insert("path".into(), json!(entry.path.clone()));
    response.insert("timestamp".into(), json!(timestamp));
    response.insert("db".into(), json!(target_db.as_str()));
    let status = if enrichment_enqueued {
        "saved (enrichment pending)"
    } else {
        "saved"
    };
    response.insert("status".into(), json!(status));
    response.insert(
        "enrichment".into(),
        json!({
            "queued": enrichment_enqueued,
            "embedding_pending": needs_embedding && entry.vector.is_none(),
            "summary_pending": needs_summary && entry.summary.is_empty(),
        }),
    );
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }
    if let Some(violations) = gate_warnings {
        response.insert("capture_gate_warnings".into(), violations);
    }
    if secret_redactions > 0 {
        response.insert("secret_redactions".into(), json!(secret_redactions));
        response.insert(
            "secret_redaction_warning".into(),
            json!("Potential secrets were redacted before persistence."),
        );
    }
    response
}

pub(crate) async fn handle_save_memory(
    server: &MemoryServer,
    mut params: SaveMemoryParams,
) -> Result<String, String> {
    params.text = scrub_think_tags(&params.text);
    params.summary = scrub_think_tags(&params.summary);
    let (safe_text, secret_redactions) = scrub_secrets(&params.text);
    let gate_warnings = match validate_save_text(&params, &safe_text)? {
        SaveTextValidation::Accepted(warnings) => warnings,
        SaveTextValidation::Rejected(body) => return Ok(body),
    };
    let requested_id = params.id.clone();
    let id = requested_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let timestamp = params
        .timestamp
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let valid_from = params
        .valid_from
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| timestamp.clone());
    let requested_scope = params.scope.clone();
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None) // Will use named project below
    } else {
        server.resolve_write_scope(&requested_scope)
    };
    let existing_revision = lookup_existing_revision(
        server,
        &id,
        requested_id.is_some(),
        target_db,
        named_project.as_deref(),
    )?;
    let enrichment_revision = existing_revision.unwrap_or(0) + 1;

    let needs_summary = params.summary.is_empty();
    let needs_embedding = params.vector.is_none();
    let auto_link = params.auto_link;
    let entry = build_save_entry(
        server,
        params,
        safe_text,
        id.clone(),
        timestamp.clone(),
        valid_from,
        target_db,
    );

    upsert_save_entry(server, &entry, target_db, named_project.as_deref())?;

    if !needs_embedding && entry.vector.is_some() {
        spawn_save_contradiction_detection(server, id, target_db, named_project.clone());
    }

    let enrichment_enqueued = enqueue_save_enrichment(
        server,
        &entry,
        needs_embedding,
        needs_summary,
        target_db,
        named_project.clone(),
        enrichment_revision,
    );
    let mut response = build_save_response(
        &entry,
        &timestamp,
        target_db,
        enrichment_enqueued,
        needs_embedding,
        needs_summary,
        warning,
        gate_warnings,
        secret_redactions,
    );

    if auto_link && !entry.entities.is_empty() && !is_training_seed(&entry) {
        spawn_auto_linking(server, &entry, target_db, named_project);
        response.insert("auto_link".into(), json!("pending"));
    } else if auto_link && !entry.entities.is_empty() && is_training_seed(&entry) {
        response.insert("auto_link".into(), json!("skipped_training_seed"));
    }

    serde_json::to_string(&serde_json::Value::Object(response))
        .map_err(|e| format!("Failed to serialize response: {}", e))
}

/// Low-friction shortcut over `handle_save_memory`. Infers `path`, `category`,
/// and `importance` so callers only need to pass `text` (and optionally
/// `tags`). Internally constructs `SaveMemoryParams` and delegates, so noise
/// filter, capture gate, provenance injection, auto-link, and the enrichment
/// batcher all run identically to a direct save_memory call.
pub(crate) async fn handle_remember(
    server: &MemoryServer,
    params: RememberParams,
) -> Result<String, String> {
    // Default path = /notes/{YYYY-MM-DD} so quick captures land in a
    // predictable, browsable bucket without forcing the caller to choose one.
    let inferred_path = params.path.unwrap_or_else(|| {
        let date = Utc::now().format("%Y-%m-%d");
        format!("/notes/{date}")
    });

    let save_params = SaveMemoryParams {
        text: params.text,
        summary: params.summary,
        path: inferred_path,
        importance: params.importance.unwrap_or(0.6).clamp(0.0, 1.0),
        category: params.category.unwrap_or_else(|| "fact".to_string()),
        topic: params.topic,
        keywords: params.tags,
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: params.scope.unwrap_or_else(|| "project".to_string()),
        vector: None,
        id: None,
        force: params.force,
        auto_link: true,
        project: params.project,
        retention_policy: params.retention_policy,
        domain: params.domain,
        timestamp: None,
        valid_from: params.valid_from,
        valid_until: params.valid_until,
        metadata: Some(json!({ "shortcut": "remember" })),
    };

    handle_save_memory(server, save_params).await
}

/// Format a save-path error string. When the underlying SQLite error indicates
/// a readonly database, attach the resolved DB path, the active scope/profile,
/// and a concrete remediation hint. Non-readonly errors fall through to the
/// previous one-line format so existing callers (and tests) keep working.
pub(crate) fn format_save_error(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    err: &dyn std::fmt::Display,
) -> String {
    let err_str = err.to_string();
    let lower = err_str.to_ascii_lowercase();
    let is_readonly = lower.contains("readonly")
        || lower.contains("read-only")
        || lower.contains("read only")
        || lower.contains("attempt to write a readonly database");

    if !is_readonly {
        return match named_project {
            Some(name) => format!("Failed to save memory to '{}': {}", name, err_str),
            None => format!("Failed to save memory: {}", err_str),
        };
    }

    let db_path = match named_project {
        Some(name) => crate::MemoryServer::resolve_named_project_db_path(name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| format!("<named project: {name}>")),
        None => match target_db {
            DbScope::Global => server.global_db_path.display().to_string(),
            DbScope::Project => server
                .project_db_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<no project DB configured>".to_string()),
        },
    };

    let profile_label = server
        .active_tool_profile()
        .map(|p| p.as_str())
        .unwrap_or_else(|| "admin".to_string());

    format!(
        "Failed to save memory: database is read-only.\n  \
         db_path: {db_path}\n  \
         scope: {scope}\n  \
         profile: {profile_label}\n  \
         hints:\n    \
         - Another process may hold an exclusive lock; check for stale `tachi` daemons.\n    \
         - File permissions may be wrong; ensure the user owns the DB file and parent dir.\n    \
         - The DB may have been opened read-only by an earlier CLI command — restart the daemon.\n    \
         - If targeting the wrong DB, pass --global-db / --project-db (or `project=` on the call).\n  \
         underlying: {err_str}",
        scope = target_db.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: "/test".into(),
            summary: text[..text.len().min(30)].into(),
            text: text.into(),
            importance: 0.7,
            timestamp: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "".into(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "test".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn should_enqueue_enrichment_always_true() {
        let e = test_entry("enr-1", "test");
        assert!(should_enqueue_enrichment(&e));
    }

    #[test]
    fn enrichment_work_pending_when_metadata_missing() {
        let mut e = test_entry("enr-2", "test");
        e.summary = "ready".into();
        e.vector = Some(vec![0.1; 64]);
        e.keywords = vec![];
        e.entities = vec![];
        assert!(enrichment_work_pending(&e, false, false));

        e.keywords = vec!["tag".into()];
        e.entities = vec!["entity".into()];
        assert!(!enrichment_work_pending(&e, false, false));
    }

    #[test]
    fn should_enqueue_enrichment_high_importance_legacy() {
        let mut e = test_entry("enr-1", "test");
        e.importance = 0.5;
        assert!(should_enqueue_enrichment(&e));
    }
}
