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

pub(in crate::copilot_ops) fn wiki_text_tokens(input: &str) -> HashSet<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.chars().count() >= 3)
        .collect()
}

pub(in crate::copilot_ops) fn wiki_subject_token(input: &str) -> Option<String> {
    let tokens = wiki_text_tokens(input);
    if tokens.len() == 1 {
        tokens.into_iter().next()
    } else {
        None
    }
}

pub(in crate::copilot_ops) fn wiki_text_jaccard_sets(
    a: &HashSet<String>,
    b: &HashSet<String>,
) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
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

/// Decide whether an active Wiki/Guide row describes the same semantic subject
/// as a canonical projection candidate.
pub(crate) fn is_wiki_projection_duplicate(
    candidate: &MemoryEntry,
    path: &str,
    topic: &str,
    text: &str,
) -> bool {
    let target_subject = wiki_subject_token(topic);
    let target_text_tokens = wiki_text_tokens(text);
    // Dedup criteria (OR-combined, but single-token topic match requires path
    // prefix overlap to avoid over-broad matching).
    let same_path = candidate.path == path;
    let same_parent = candidate.path.rsplit_once('/').map(|(parent, _)| parent)
        == path.rsplit_once('/').map(|(parent, _)| parent);
    let same_exact_topic = !topic.trim().is_empty()
        && candidate.topic.trim().eq_ignore_ascii_case(topic.trim())
        && same_parent;
    let same_subject_token = target_subject.as_ref().is_some_and(|token| {
        let candidate_token = wiki_subject_token(&candidate.topic);
        candidate_token.as_ref() == Some(token) && same_parent
    });
    let similar_text =
        wiki_text_jaccard_sets(&target_text_tokens, &wiki_text_tokens(&candidate.text))
            >= WIKI_DUP_JACCARD_THRESHOLD;
    same_path || same_exact_topic || same_subject_token || similar_text
}

pub(crate) fn wiki_projection_supersedes_edge(
    canonical_id: &str,
    candidate_id: &str,
    path: &str,
    topic: &str,
    created_at: &str,
) -> memcore::MemoryEdge {
    memcore::MemoryEdge {
        source_id: canonical_id.to_string(),
        target_id: candidate_id.to_string(),
        relation: "supersedes".to_string(),
        weight: 0.9,
        metadata: json!({
            "source": "wiki_write_dedup",
            "path": path,
            "topic": topic,
        }),
        created_at: created_at.to_string(),
        valid_from: String::new(),
        valid_to: None,
    }
}

pub(crate) fn wiki_parent_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/wiki")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiki_candidate(id: &str, path: &str, topic: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: path.to_string(),
            summary: text.to_string(),
            text: text.to_string(),
            importance: 0.7,
            timestamp: "2026-07-31T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "experience".to_string(),
            topic: topic.to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "wiki".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: json!({"wiki": true}),
            vector: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("wiki".to_string()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn single_token_topic_requires_the_same_parent_to_be_a_duplicate() {
        let candidate = wiki_candidate(
            "ops-mcp",
            "/wiki/ops/mcp",
            "mcp",
            "Operational transport retry notes with no engineering overlap.",
        );

        assert!(!is_wiki_projection_duplicate(
            &candidate,
            "/wiki/engineering/mcp",
            "mcp",
            "Engineering protocol schema conventions for tool interoperability.",
        ));
        assert!(is_wiki_projection_duplicate(
            &candidate,
            "/wiki/ops/mcp-replacement",
            "mcp",
            "A replacement operational transport note.",
        ));
    }
}
