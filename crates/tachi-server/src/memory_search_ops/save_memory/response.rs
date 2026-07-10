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
    let mut enrichment = serde_json::Map::new();
    enrichment.insert("queued".into(), json!(enrichment_enqueued));
    enrichment.insert(
        "embedding_pending".into(),
        json!(needs_embedding && entry.vector.is_none()),
    );
    enrichment.insert(
        "summary_pending".into(),
        json!(needs_summary && entry.summary.is_empty()),
    );
    // Write-side keyword enrichment status (#921). Only surface when the
    // feature flag is on so flag-off remains zero behavior change.
    if crate::enrichment::write_keyword_enrichment_enabled() {
        let keywords_status = entry
            .metadata
            .get("enrichment")
            .and_then(|value| value.get("keywords_status"))
            .and_then(|value| value.as_str())
            .unwrap_or(
                if enrichment_enqueued && crate::enrichment::needs_keyword_enrichment(entry) {
                    "pending"
                } else {
                    "skipped"
                },
            );
        enrichment.insert("keywords_status".into(), json!(keywords_status));
        enrichment.insert(
            "keywords_pending".into(),
            json!(keywords_status == "pending"),
        );
    }
    response.insert("enrichment".into(), json!(enrichment));
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
