//! Business logic for the `tachi_save` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_save`].

use crate::copilot_ops::handle_tachi_wiki_write;
use crate::facade_memory_ops::shape_save_facade_response;
use crate::memory_search_ops::{handle_remember, handle_save_memory};
use crate::pipeline_ops::handle_extract_facts;
use crate::tool_params::*;
use crate::MemoryServer;

pub(crate) async fn handle_tachi_save(
    server: &MemoryServer,
    params: TachiSaveParams,
) -> Result<String, String> {
    let kind = params.kind.as_deref().unwrap_or("").to_ascii_lowercase();

    if kind == "facts" || kind == "extract_facts" {
        let extract_params = ExtractFactsParams {
            text: params.text.clone(),
            source: params
                .source
                .clone()
                .unwrap_or_else(|| "tachi_save".to_string()),
            project: params.project.clone(),
        };
        return handle_extract_facts(server, extract_params).await;
    }

    // Detect scope=note: even if kind is empty, treat as note when scope="note"
    let scope_is_note = params
        .scope
        .as_deref()
        .map(|s| s.eq_ignore_ascii_case("note"))
        .unwrap_or(false);

    // Auto-detect: explicit DB-style path (starts with '/') signals memory,
    // wiki when caller provides a title, otherwise short text is treated as
    // a quick note. Path shape wins over text length so callers passing
    // memory paths like "/scratch/foo" don't get bounced to the note writer.
    let path_looks_like_db_path = params
        .path
        .as_deref()
        .map(|p| p.starts_with('/'))
        .unwrap_or(false);
    let resolved_kind = if kind.is_empty() {
        if scope_is_note {
            "note"
        } else if params.title.is_some() {
            "wiki"
        } else if path_looks_like_db_path {
            "memory"
        } else if params.text.chars().count() < 200 {
            "note"
        } else {
            "memory"
        }
    } else {
        &kind
    };

    match resolved_kind {
        "wiki" => {
            let title = params
                .title
                .clone()
                .unwrap_or_else(|| "Untitled".to_string());
            let wiki_params = WikiWriteParams {
                title,
                text: params.text.clone(),
                path: params.path.clone(),
                topic: params.topic.clone(),
                summary: params.summary.clone(),
                category: params
                    .category
                    .clone()
                    .unwrap_or_else(|| "experience".to_string()),
                keywords: params.keywords.clone(),
                entities: params.entities.clone(),
                importance: params.importance.unwrap_or(0.85),
                scope: params.scope.clone().unwrap_or_else(|| "global".to_string()),
                retention_policy: params
                    .retention_policy
                    .clone()
                    .unwrap_or_else(|| "permanent".to_string()),
                domain: params.domain.clone(),
                project: params.project.clone(),
                metadata: params.metadata.clone(),
                force: params.force,
                references: params.references.clone(),
                include_patterns: false,
                pattern_query: None,
                pattern_top_k: None,
            };
            handle_tachi_wiki_write(server, wiki_params).await
        }
        "note" => {
            let (abs_note_path, rel_note_path) = crate::notes_ops::write_note_file(
                &params.text,
                params.path.as_deref(),
                params.title.as_deref(),
                params.topic.as_deref(),
                params.category.as_deref(),
                &params.keywords,
            )?;

            let db_path = format!("/notes/{}", rel_note_path);
            let db_scope = params
                .scope
                .as_deref()
                .filter(|s| !s.eq_ignore_ascii_case("note"))
                .unwrap_or("project")
                .to_string();

            let remember_params = RememberParams {
                text: params.text.clone(),
                summary: params.summary.clone().unwrap_or_default(),
                tags: params.keywords.clone(),
                topic: params.topic.clone().unwrap_or_default(),
                importance: params.importance,
                scope: Some(db_scope),
                project: params.project.clone(),
                project_explicit: params.project_explicit,
                path: Some(db_path),
                category: Some(
                    params
                        .category
                        .clone()
                        .unwrap_or_else(|| "note".to_string()),
                ),
                domain: params.domain.clone(),
                retention_policy: params
                    .retention_policy
                    .clone()
                    .or_else(|| Some("durable".to_string())),
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                force: params.force,
            };
            let mut result_str = handle_remember(server, remember_params).await?;

            if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&result_str) {
                if let Some(obj) = val.as_object_mut() {
                    obj.insert(
                        "note_file".to_string(),
                        serde_json::json!(abs_note_path.to_string_lossy()),
                    );
                    obj.insert("note_path".to_string(), serde_json::json!(rel_note_path));
                }
                result_str = serde_json::to_string(&val)
                    .map_err(|e| format!("serialize note result: {e}"))?;
            }

            Ok(result_str)
        }
        _ => {
            // "memory" or any other value
            let metadata =
                merge_referenced_files(params.metadata.clone(), &params.files, &params.text);
            let mem_params = SaveMemoryParams {
                text: params.text.clone(),
                summary: params.summary.clone().unwrap_or_default(),
                path: params.path.clone().unwrap_or_else(|| "/".to_string()),
                importance: params.importance.unwrap_or(0.7),
                category: params
                    .category
                    .clone()
                    .unwrap_or_else(|| "fact".to_string()),
                topic: params.topic.clone().unwrap_or_default(),
                keywords: params.keywords.clone(),
                persons: Vec::new(), // legacy wire field; MCP uses entities for people
                entities: params.entities.clone(),
                location: String::new(),
                scope: params
                    .scope
                    .clone()
                    .unwrap_or_else(|| "project".to_string()),
                vector: None,
                id: params.id.clone(),
                force: params.force,
                auto_link: true,
                project: params.project.clone(),
                project_explicit: params.project_explicit,
                retention_policy: params.retention_policy.clone(),
                domain: params.domain.clone(),
                timestamp: None,
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                metadata,
                emit_continuity: params.emit_continuity,
            };
            handle_save_memory(server, mem_params).await
        }
    }
}

pub(crate) fn finalize_tachi_save_response(
    params: &TachiSaveParams,
    raw: &str,
    echo: Option<&str>,
) -> Result<String, String> {
    shape_save_facade_response(
        raw,
        params.format.as_deref(),
        echo.or(Some(params.text.as_str())),
        params.path.as_deref(),
    )
}

/// Merge explicit `files` plus any `spec:`-pointer paths parsed from `text`
/// into `metadata.files` (a de-duplicated string array). Returns the metadata
/// unchanged when there are no referenced files to record, so old behaviour and
/// payloads stay byte-identical.
fn merge_referenced_files(
    metadata: Option<serde_json::Value>,
    explicit_files: &[String],
    text: &str,
) -> Option<serde_json::Value> {
    // Ordered de-dup: preserve insertion order, drop blanks/duplicates.
    let mut files: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let f = raw.trim();
        if !f.is_empty() && !files.iter().any(|existing| existing == f) {
            files.push(f.to_string());
        }
    };

    // 1. Carry forward any files already present on the incoming metadata.
    if let Some(serde_json::Value::Array(existing)) = metadata
        .as_ref()
        .and_then(|m| m.get("files").cloned())
        .as_ref()
    {
        for v in existing {
            if let Some(s) = v.as_str() {
                push(s);
            }
        }
    }
    // 2. Explicit caller-supplied files.
    for f in explicit_files {
        push(f);
    }
    // 3. Conservative auto-parse: only `spec:` pointer lines from the text.
    for path in parse_spec_pointers(text) {
        push(&path);
    }

    if files.is_empty() {
        return metadata;
    }

    let mut obj = match metadata {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("files".to_string(), serde_json::json!(files));
    Some(serde_json::Value::Object(obj))
}

/// Extract referenced file paths from `spec:` pointer lines, e.g.
/// `spec: docs/SPEC.md` or `spec:docs/SPEC.md, src/lib.rs`. Conservative by
/// design — only lines whose first non-space token is `spec:` are considered,
/// so prose mentioning the word "spec" elsewhere is never harvested.
fn parse_spec_pointers(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix("spec:")
            .or_else(|| trimmed.strip_prefix("Spec:"))
            .or_else(|| trimmed.strip_prefix("SPEC:"))
        else {
            continue;
        };
        for token in rest.split(',') {
            let path = token.trim();
            if !path.is_empty() {
                out.push(path.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod referenced_files_tests {
    use super::{merge_referenced_files, parse_spec_pointers};
    use serde_json::json;

    #[test]
    fn parses_spec_pointer_lines_only() {
        let text =
            "Decision recorded.\nspec: docs/SPEC.md, src/lib.rs\nWe also spec-checked things.";
        assert_eq!(
            parse_spec_pointers(text),
            vec!["docs/SPEC.md".to_string(), "src/lib.rs".to_string()]
        );
    }

    #[test]
    fn no_pointers_leaves_metadata_untouched() {
        // No explicit files, no spec: line → metadata returned as-is (None).
        assert!(merge_referenced_files(None, &[], "just prose, no pointers").is_none());
    }

    #[test]
    fn merges_explicit_existing_and_parsed_deduped() {
        let metadata = Some(json!({ "tier": "raw", "files": ["docs/A.md"] }));
        let explicit = vec!["src/b.rs".to_string(), "docs/A.md".to_string()];
        let merged = merge_referenced_files(metadata, &explicit, "spec: docs/C.md, src/b.rs")
            .expect("metadata present");
        assert_eq!(merged["tier"], json!("raw"));
        assert_eq!(
            merged["files"],
            json!(["docs/A.md", "src/b.rs", "docs/C.md"])
        );
    }
}
