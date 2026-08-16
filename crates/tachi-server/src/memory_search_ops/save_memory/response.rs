//! `save_memory` response contract (tachi#1435 slice 3 / #2059).
//!
//! A bare-text `save_memory` call returns lexical visibility immediately: the
//! FTS/keyword index a plain `search_memory` reads is populated synchronously
//! by the same upsert the receipt reports as `"saved"`, and (when the recall
//! cache is enabled) the write-side invalidation in `handler::handle_save_memory`
//! has already run before this response is built, so a cached search result
//! cannot mask the row that was just written. Semantic (vector-similarity)
//! visibility trails behind until embedding enrichment finishes — poll
//! `get_memory` on the returned `confirm.id` (scoped by `confirm.project` when
//! present) until its `enrichment.embedding_pending` (mirrored here as
//! `visibility.semantic == "immediate"`) reads false. Any caller re-querying
//! this entry must target the same `read_target.scope`/`read_target.project`
//! this receipt reports, not the scope it originally requested — write-affinity
//! routing may have placed the row somewhere else.

use crate::DbScope;
use memcore::MemoryEntry;
use serde_json::json;

pub(in crate::memory_search_ops::save_memory) fn build_save_response(
    entry: &MemoryEntry,
    timestamp: &str,
    target_db: DbScope,
    named_project: Option<&str>,
    enrichment_enqueued: bool,
    needs_embedding: bool,
    needs_summary: bool,
    warning: Option<String>,
    gate_warnings: Option<serde_json::Value>,
    secret_redactions: usize,
    requested_scope: &str,
    scope_warning: Option<String>,
    recall_fence: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut response = serde_json::Map::new();
    response.insert("id".into(), json!(entry.id.clone()));
    response.insert("path".into(), json!(entry.path.clone()));
    response.insert("timestamp".into(), json!(timestamp));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("status".into(), json!("saved"));
    // #1435 slice 3 / #2059: the save-visibility contract. `lexical` is
    // always "immediate" — the FTS/keyword index a plain-text `search_memory`
    // call reads is populated synchronously by the same upsert that just
    // committed. `semantic` mirrors the existing `embedding_pending` signal
    // below: embedding enrichment runs async, so vector-similarity recall
    // only catches up once that finishes.
    let embedding_pending = needs_embedding && entry.vector.is_none();
    response.insert(
        "visibility".into(),
        json!({
            "lexical": "immediate",
            "semantic": if embedding_pending { "pending" } else { "immediate" },
        }),
    );
    // `read_target`: exactly where this entry landed, so a caller doesn't
    // have to reverse-engineer write-affinity routing to know which
    // scope/project a follow-up search or `get_memory` must target.
    let mut read_target = serde_json::Map::new();
    read_target.insert("scope".into(), json!(target_db.as_str()));
    if let Some(project) = named_project {
        read_target.insert("project".into(), json!(project));
    }
    response.insert("read_target".into(), serde_json::Value::Object(read_target));
    // `recall_fence`: whether the recall-cache write-side invalidation that
    // just ran (see `handler::handle_save_memory`) actually cleared any stale
    // cached search result that could otherwise mask this save. "disabled"
    // when the recall cache itself is off (nothing to clear); "unconfirmed"
    // is a loud degrade, never a silent one, when the DELETE failed after the
    // save had already committed.
    response.insert("recall_fence".into(), json!(recall_fence));
    // `confirm`: a ready-made pointer at the one endpoint that authoritatively
    // answers "is this row visible yet" — reuses `get_memory` rather than
    // minting a new one.
    let mut confirm = serde_json::Map::new();
    confirm.insert("tool".into(), json!("get_memory"));
    confirm.insert("id".into(), json!(entry.id.clone()));
    if let Some(project) = named_project {
        confirm.insert("project".into(), json!(project));
    }
    response.insert("confirm".into(), serde_json::Value::Object(confirm));
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
    // #1041 F6: this used to say "Pass force=true to write anyway", which
    // stopped being true once S3 decoupled dedup from the noise/capture-gate
    // bypass — `force` skips content-quality gates only, never this
    // path+text identity check. The only way to force a second, distinct
    // row is a caller-supplied `id` (the dedup check only runs when `id` is
    // absent in the first place).
    response.insert(
        "hint".into(),
        json!(
            "Identical text already saved at this path. force=true does NOT bypass this — pass your own explicit id= to create a second, distinct row anyway."
        ),
    );
    response
}
