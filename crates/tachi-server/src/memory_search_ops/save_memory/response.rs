use crate::DbScope;
use memcore::MemoryEntry;
use serde_json::json;

pub(in crate::memory_search_ops::save_memory) fn build_save_response(
    entry: &MemoryEntry,
    timestamp: &str,
    target_db: DbScope,
    enrichment_enqueued: bool,
    needs_embedding: bool,
    needs_summary: bool,
    warning: Option<String>,
    gate_warnings: Option<serde_json::Value>,
    secret_redactions: usize,
    requested_scope: &str,
    scope_warning: Option<String>,
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
    // #925: loud, dedicated fallback signal — only present when the
    // effective scope differs from what was requested (e.g. `scope=project`
    // silently downgraded to global on a single-DB daemon). Absent entirely
    // when the requested scope was honored, so callers can treat presence
    // of `scope_warning` as the mismatch signal.
    if let Some(scope_warning) = scope_warning {
        response.insert("scope".into(), json!(requested_scope));
        response.insert("scope_warning".into(), json!(scope_warning));
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

pub(in crate::memory_search_ops::save_memory) fn build_duplicate_save_response(
    existing_id: &str,
    path: &str,
    target_db: DbScope,
) -> serde_json::Map<String, serde_json::Value> {
    let mut response = serde_json::Map::new();
    response.insert("saved".into(), json!(false));
    response.insert("status".into(), json!("duplicate"));
    response.insert("id".into(), json!(existing_id));
    response.insert("path".into(), json!(path));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert(
        "hint".into(),
        json!("Identical text already saved at this path. Pass force=true to write anyway."),
    );
    response
}
