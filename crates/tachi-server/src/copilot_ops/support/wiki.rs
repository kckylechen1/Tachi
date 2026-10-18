use super::*;

pub(in crate::copilot_ops) fn normalize_wiki_path(path: Option<String>, topic: &str) -> String {
    let raw = path
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("/wiki/general/{}", wiki_slug(topic)));
    let with_slash = if raw.starts_with('/') {
        raw
    } else {
        format!("/{raw}")
    };
    let namespaced = if with_slash == "/wiki"
        || with_slash.starts_with("/wiki/")
        || with_slash == "/guide"
        || with_slash.starts_with("/guide/")
    {
        with_slash
    } else {
        format!("/wiki{}", with_slash)
    };
    memcore::path_router::normalize_path(&namespaced)
}

/// Builds the canonical candidate artifact metadata stamped onto every
/// `tachi_wiki_write` entry. Semantic scope comes only from the scope request;
/// physical `project=` placement is intentionally not an input.
///
/// Public `tachi_wiki_write` input cannot establish human review authority.
/// Metadata and references are caller-controlled, so every write through this
/// boundary remains `pending_review` regardless of path or supplied receipt.
/// References are retained only as provenance. A future approval flow must use
/// a separate server-verified typed authority channel.
///
/// `/wiki/drafts/...` stays `pending_review` unconditionally; the REM evolver
/// uses this same candidate seam and then records its workflow review marker.
pub(in crate::copilot_ops) fn wiki_layer_metadata(
    path: &str,
    scope: &str,
    references: &[String],
    proposal_metadata: &Value,
) -> Value {
    let is_guide = path == "/guide" || path.starts_with("/guide/");
    let layer = if is_guide { "guide" } else { "wiki" };
    let candidate = build_candidate_knowledge_artifact_fields(path, scope, proposal_metadata);
    let mut obj = candidate.as_object().cloned().unwrap_or_default();
    let is_shared = obj
        .get("knowledge_scope")
        .and_then(Value::as_str)
        .is_some_and(|scope| scope == "shared");
    let legacy_scope = if is_shared {
        "global".to_string()
    } else {
        obj.get("knowledge_scope")
            .and_then(Value::as_str)
            .unwrap_or("unspecified")
            .to_string()
    };
    obj.insert("layer".to_string(), json!(layer));
    obj.insert("scope".to_string(), json!(legacy_scope));
    // Legacy field, kept for back-compat; nothing in this codebase reads
    // it today (confirmed by repo-wide grep), but it must stay truthful
    // rather than the old hardcoded "active" now that draft paths exist.
    obj.insert(
        "status".to_string(),
        json!(WikiLifecycleV1::PendingReview.as_str()),
    );
    obj.insert("source_ref".to_string(), json!(references.first().cloned()));
    Value::Object(obj)
}

pub(in crate::copilot_ops) fn find_wiki_entry_by_path(
    store: &mut MemoryStore,
    path: &str,
) -> Result<Option<MemoryEntry>, String> {
    memcore::db::find_active_wiki_entry_by_path(store.connection(), path)
        .map_err(|e| format!("wiki existing lookup: {e}"))
}

pub(in crate::copilot_ops) fn with_existing_wiki_store<T>(
    server: &MemoryServer,
    project_name: &str,
    use_named_project: bool,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if use_named_project {
        server.with_named_project_store_identity_checked(project_name, f)
    } else {
        server.with_global_store_identity_checked(f)
    }
}

pub(in crate::copilot_ops) fn default_named_project_available(
    server: &MemoryServer,
    project_name: &str,
) -> bool {
    let Ok(db_path) = server.resolve_server_named_project_db_path(project_name) else {
        return false;
    };
    let Some(app_home) = db_path
        .parent()
        .and_then(|project_dir| project_dir.parent())
        .and_then(|projects_dir| projects_dir.parent())
    else {
        return false;
    };
    let app_home = std::fs::canonicalize(app_home).unwrap_or_else(|_| app_home.to_path_buf());
    let server_home =
        std::fs::canonicalize(server.tachi_home_dir()).unwrap_or_else(|_| server.tachi_home_dir());
    server.global_db_path_buf().starts_with(&app_home) || server_home == app_home
}
