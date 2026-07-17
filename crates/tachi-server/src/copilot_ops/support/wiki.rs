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
    if with_slash == "/wiki"
        || with_slash.starts_with("/wiki/")
        || with_slash == "/guide"
        || with_slash.starts_with("/guide/")
    {
        with_slash
    } else {
        format!("/wiki{}", with_slash)
    }
}

/// Builds the layer/scope/authority/lifecycle metadata stamped onto every
/// `tachi_wiki_write` entry, plus the #1072 dual-write typed `evidence_refs_v1`
/// (canon doc §7.1) alongside the legacy `metadata.source_refs: string[]`
/// (set separately by the caller — this function never touches that key, so
/// `source_refs` is never mutated in place).
///
/// Cross-vendor review fix (#1215, BUG 1): `lifecycle` NO LONGER defaults to
/// `active` for every non-draft path. Canon doc §7's invariant is
/// `Active ⇒ validated sources + approval` — a write with no caller-supplied
/// `review_receipt` and no source references is, by definition, neither
/// reviewed nor sourced, so it must land as `pending_review` regardless of
/// path. This also restores the frozen contract's own top sentence: wiki
/// "cannot become project truth by retention or model output" — an ordinary
/// `tachi_wiki_write` call (agent-authored, no receipt) is model output.
/// `/wiki/drafts/...` stays `pending_review` unconditionally (the one
/// draft-path convention this leaf's own writer is aware of;
/// `foundry_runtime_ops::wiki_evolver`'s weekly REM synthesis writes drafts
/// through a *different* path — `tachi_save` directly — and stamps its own
/// `metadata.review_status = "pending"` marker after the fact; the read-side
/// gate (`derive_wiki_lifecycle`) honors both origins).
///
/// A write becomes `active` only when `caller_metadata` carries an
/// `approved` `review_receipt` (canon doc §7's `WikiReviewReceiptV1`) AND at
/// least one validated source reference is present — both halves of the
/// invariant, not just one. When both hold, `source_bundle_hash` is stamped
/// from the references (a real, checkable hash — see canon doc's
/// `KnowledgeArtifactV1.source_bundle_hash`), not left `None`/omitted. No
/// live caller in this leaf currently supplies an approved `review_receipt`
/// (there is no review/approval MCP action yet) — that is intentional: it
/// means every write through this leaf's `tachi_wiki_write` lands
/// `pending_review` until a future leaf builds the approval surface, exactly
/// matching "cannot become project truth by ... model output."
pub(in crate::copilot_ops) fn wiki_layer_metadata(
    path: &str,
    scope: &str,
    project: Option<&str>,
    references: &[String],
    caller_metadata: Option<&Value>,
) -> Value {
    let is_guide = path == "/guide" || path.starts_with("/guide/");
    let layer = if is_guide { "guide" } else { "wiki" };
    let scope = if project.is_some() || scope.eq_ignore_ascii_case("project") {
        "project"
    } else {
        "global"
    };
    let authority = if is_guide {
        WikiAuthorityV1::Playbook
    } else {
        WikiAuthorityV1::Advisory
    };
    let is_draft_path = path == "/wiki/drafts" || path.starts_with("/wiki/drafts/");
    let has_validated_sources = !references.is_empty();
    let approved_review_receipt: Option<WikiReviewReceiptV1> = caller_metadata
        .and_then(|meta| meta.get("review_receipt"))
        .and_then(|value| serde_json::from_value::<WikiReviewReceiptV1>(value.clone()).ok())
        .filter(|receipt| receipt.decision.eq_ignore_ascii_case("approved"));
    let lifecycle = if is_draft_path {
        WikiLifecycleV1::PendingReview
    } else if has_validated_sources && approved_review_receipt.is_some() {
        WikiLifecycleV1::Active
    } else {
        WikiLifecycleV1::PendingReview
    };
    let artifact_kind = if is_guide {
        WikiArtifactKindV1::Guide
    } else if is_draft_path {
        WikiArtifactKindV1::Draft
    } else {
        WikiArtifactKindV1::Wiki
    };
    let captured_at = Utc::now().to_rfc3339();
    let evidence_refs_v1 = build_evidence_refs_v1(references, &captured_at);
    let source_bundle_hash = (lifecycle == WikiLifecycleV1::Active && has_validated_sources)
        .then(|| canonical_json_sha256(&json!(references)).ok())
        .flatten();
    let mut obj = serde_json::Map::new();
    obj.insert("layer".to_string(), json!(layer));
    obj.insert("scope".to_string(), json!(scope));
    obj.insert("authority".to_string(), json!(authority.as_str()));
    // Legacy field, kept for back-compat; nothing in this codebase reads
    // it today (confirmed by repo-wide grep), but it must stay truthful
    // rather than the old hardcoded "active" now that draft paths exist.
    obj.insert("status".to_string(), json!(lifecycle.as_str()));
    obj.insert("lifecycle".to_string(), json!(lifecycle.as_str()));
    obj.insert("artifact_kind".to_string(), json!(artifact_kind.as_str()));
    obj.insert("source_ref".to_string(), json!(references.first().cloned()));
    obj.insert("evidence_refs_v1".to_string(), json!(evidence_refs_v1));
    if let Some(hash) = source_bundle_hash {
        obj.insert("source_bundle_hash".to_string(), json!(hash));
    }
    if let Some(receipt) = &approved_review_receipt {
        if lifecycle == WikiLifecycleV1::Active {
            obj.insert(
                "review_receipt".to_string(),
                serde_json::to_value(receipt).unwrap_or(Value::Null),
            );
        }
    }
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

pub(in crate::copilot_ops) fn find_wiki_entry_by_path_or_topic(
    store: &mut MemoryStore,
    path: &str,
    topic: &str,
) -> Result<Option<MemoryEntry>, String> {
    memcore::db::find_active_wiki_entry_by_path_or_topic(store.connection(), path, topic)
        .map_err(|e| format!("wiki existing lookup: {e}"))
}

pub(in crate::copilot_ops) fn with_existing_wiki_store<T>(
    server: &MemoryServer,
    project_name: &str,
    use_named_project: bool,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if use_named_project {
        server.with_named_project_store(project_name, f)
    } else {
        server.with_global_store(f)
    }
}

pub(in crate::copilot_ops) fn default_named_project_available(
    server: &MemoryServer,
    project_name: &str,
) -> bool {
    let Ok(db_path) = MemoryServer::resolve_named_project_db_path(project_name) else {
        return false;
    };
    let Some(app_home) = db_path
        .parent()
        .and_then(|project_dir| project_dir.parent())
        .and_then(|projects_dir| projects_dir.parent())
    else {
        return false;
    };
    server.global_db_path_buf().starts_with(app_home)
}

/// Identify and supersede wiki entries that duplicate the newly written entry.
pub(in crate::copilot_ops) fn supersede_wiki_duplicates(
    store: &mut MemoryStore,
    canonical_id: &str,
    path: &str,
    topic: &str,
    text: &str,
) -> Result<usize, String> {
    let parent_path = wiki_parent_path(path);
    let candidates = store
        .list_wiki_duplicate_candidates(path, topic, &parent_path, 500)
        .map_err(|e| format!("wiki duplicate scan: {e}"))?;
    let mut changed = 0usize;
    let target_subject = wiki_subject_token(topic);
    let target_text_tokens = wiki_text_tokens(text);
    for candidate in candidates {
        if candidate.id == canonical_id {
            continue;
        }
        // Dedup criteria (OR-combined, but single-token topic match requires path prefix overlap)
        let same_path = candidate.path == path;
        let same_topic = target_subject.as_ref().is_some_and(|token| {
            let cand_token = wiki_subject_token(&candidate.topic);
            cand_token.as_ref() == Some(token)
                // Single-token topics require path prefix overlap to avoid over-broad matching
                && (token.len() > 1
                    || candidate.path.rsplit_once('/').map(|(parent, _)| parent) == path.rsplit_once('/').map(|(parent, _)| parent))
        });
        let similar_text =
            wiki_text_jaccard_sets(&target_text_tokens, &wiki_text_tokens(&candidate.text))
                >= WIKI_DUP_JACCARD_THRESHOLD;
        let same_subject = same_path || same_topic || similar_text;
        if !same_subject {
            continue;
        }
        if store
            .supersede_memory(&candidate.id, canonical_id)
            .map_err(|e| format!("wiki duplicate supersede: {e}"))?
        {
            let edge = memcore::MemoryEdge {
                source_id: canonical_id.to_string(),
                target_id: candidate.id.clone(),
                relation: "supersedes".to_string(),
                weight: 0.9,
                metadata: json!({
                    "source": "wiki_write_dedup",
                    "path": path,
                    "topic": topic,
                }),
                created_at: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            };
            let _ = store.add_edge(&edge);
            changed += 1;
        }
    }
    Ok(changed)
}

pub(in crate::copilot_ops) fn wiki_parent_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/wiki")
        .to_string()
}
