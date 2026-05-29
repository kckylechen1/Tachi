//! Business logic for the `tachi_save` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_save`].

use crate::copilot_ops::handle_tachi_wiki_write;
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
                force: params.force,
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
                persons: Vec::new(), // legacy DB column; MCP uses entities for people
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
                retention_policy: params.retention_policy.clone(),
                domain: params.domain.clone(),
                timestamp: None,
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                metadata: None,
            };
            handle_save_memory(server, mem_params).await
        }
    }
}
